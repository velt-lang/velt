//! Field reads through interface values (`x.f` with `x: I` or `x: T extends I`), and the
//! reads of arrays of interface values through an interface at a dynamic argument (`kids` of a
//! `Tree<any>`, or a method's result of such a type; docs/reference/types.md "Generic
//! interfaces with `unknown` arguments"). An adapter from another instance of the interface
//! (`iface_adapt`) reads such an array as a new array of its elements, each adapted
//! (`adapt_array`): a change made through that array, an alias of it or its identity would not
//! be Node's. Once every body is checked and every adapter made, each such read of a slot an
//! adapter reads that way is an error unless the array is used in place without changing it
//! ([`FnCx::array_used_in_place`]: an element, `.length`, `for…of` over its elements, a method
//! that does not change it, behind `!` or `?.`, a condition or a `null` test).

use rustc_hash::FxHashMap;
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Ctx;
use crate::hir::{self, Callee, DefId, ExprKind as H, TyId, TyKind, UseMode};
use crate::ide::record::Target;

/// The reads of arrays of interface values through interfaces at dynamic arguments (a field,
/// or a method's result), and the slots adapters read as new arrays.
#[derive(Default)]
pub(crate) struct ArrayReads {
    /// By the read's span. A read checked more than once (a check made to learn a type, then
    /// the real one) keeps what its last check found.
    reads: FxHashMap<Span, ArrayRead>,
    /// `(interface, slot)`: an adapter reads the field or method result as a new array; the
    /// conversion making the first such adapter.
    adapted: FxHashMap<(DefId, u32), Span>,
    /// The span of an expression whose value is a read's (`t.kids!`), to the read's.
    same: FxHashMap<Span, Span>,
}

/// A read of an array of interface values through slot `slot` of `iface`.
struct ArrayRead {
    iface: DefId,
    slot: u32,
    /// What is read (`` `kids` read ``, `` `getKids()` called ``) and the interface type it is
    /// read through, as shown.
    what: String,
    through: String,
    /// It is used in place without being changed.
    in_place: bool,
}

impl FnCx<'_, '_> {
    /// `x.field` on an interface value or bounded generic: a call of the field's getter slot
    /// (see `collect::getters`); the result is an owned copy / clone. Writes go through the
    /// setter slot (`iface_write`), never through this place.
    pub(super) fn iface_field(
        &mut self,
        mut obj: hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(f) = self.iface_field_of(obj.ty, &prop.name) else {
            return Err(obj);
        };
        let slot = self.cx.iface(f.iface).map_or(0, |i| i.methods.len()) + f.index;
        self.cx
            .rec_ref(prop.span, Target::Field(f.iface, f.index as u32));
        if want == Want::BorrowMut {
            self.cx.err(
                format!(
                    "cannot modify `{}` through an interface or generic value",
                    prop.name
                ),
                prop.span,
            );
            return Ok(self.error_expr(span));
        }
        set_place_mode(&mut obj, UseMode::Borrow);
        let what = format!("`{}` read", prop.name);
        self.note_iface_array_read(f.iface, &f.args, (slot as u32, what), f.ty, span);
        let callee = if f.on_param {
            Callee::ParamMethod {
                iface: f.iface,
                iface_args: f.args,
                slot: slot as u32,
                method_type_args: vec![],
            }
        } else {
            Callee::Dyn { slot: slot as u32 }
        };
        let kind = H::Call {
            callee,
            args: vec![obj],
        };
        Ok(self.mk(kind, f.ty, span))
    }

    /// Slot `slot` (`what` it reads) of interface `iface` at arguments `args` gives a `ty` at
    /// `span`: recorded when that is an array of interface values and an argument is dynamic
    /// or mentions a type parameter (generic code is checked once for every instance, and an
    /// instance at `any` may read an adapter).
    pub(super) fn note_iface_array_read(
        &mut self,
        iface: DefId,
        args: &[TyId],
        (slot, what): (u32, String),
        ty: TyId,
        span: Span,
    ) {
        // An optional field (`kids?: Tree<T>[]`) too.
        let ty = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        let TyKind::Array(elem) = *self.cx.ty.kind(ty) else {
            return;
        };
        if !matches!(self.cx.ty.kind(elem), TyKind::Dyn(..))
            || !args.iter().any(|a| {
                self.cx.dynamic(*a).is_some() || super::iface_adapt::has_params(self.cx, *a)
            })
        {
            return;
        }
        let through = self.cx.ty.intern(TyKind::Dyn(iface, args.to_vec()));
        let through = self.cx.display(through);
        let reads = &mut self.cx.iface_writes.array_reads.reads;
        let read = ArrayRead {
            iface,
            slot,
            what,
            through,
            in_place: false,
        };
        reads.insert(span, read);
    }

    /// `h` is used in place without being changed (an element read, `.length`, `for…of` over
    /// its elements, a method that does not change it): allowed when it is a read of an
    /// adapted array field.
    pub(crate) fn array_used_in_place(&mut self, h: &hir::Expr) {
        let span = match h.kind {
            H::Call {
                callee: Callee::Dyn { .. } | Callee::ParamMethod { .. },
                ..
            } => h.span,
            // The binding an optional chain makes of its object (`t.kids?.length`), declared
            // at the object.
            H::Local(l, _) => match self.f.locals.get(l.0 as usize) {
                Some(d) if d.name == "<chain>" => d.span,
                _ => return,
            },
            // `t.kids!.length` on an optional field.
            _ => match self.cx.iface_writes.array_reads.same.get(&h.span) {
                Some(&read) => read,
                None => return,
            },
        };
        if let Some(read) = self.cx.iface_writes.array_reads.reads.get_mut(&span) {
            read.in_place = true;
        }
    }

    /// The value at `outer` is the read at `read`, when that is a read of an array of
    /// interface values (`t.kids!`).
    pub(super) fn same_array_read(&mut self, outer: Span, read: Span) {
        let r = &mut self.cx.iface_writes.array_reads;
        if outer != read && r.reads.contains_key(&read) {
            r.same.insert(outer, read);
        }
    }

    /// A call of array method `name` with `args` on `recv`: in place when the method leaves
    /// the array unchanged and does not pass it on (a callback taking a third parameter, the
    /// array itself, does).
    pub(crate) fn array_method_in_place(
        &mut self,
        recv: &hir::Expr,
        name: &str,
        args: &[ast::Expr],
    ) {
        if !IN_PLACE_METHODS.contains(&name) {
            return;
        }
        // The array is the callback's fourth parameter for `reduce`, its third otherwise
        // (std does not pass it yet, #1441).
        let before = 2 + usize::from(name.starts_with("reduce"));
        let passes_array = args.iter().any(|a| match &a.kind {
            ast::ExprKind::Arrow { params, .. } => params.len() > before,
            ast::ExprKind::Function(f) => f.sig.params.len() > before,
            _ => false,
        });
        if !passes_array {
            self.array_used_in_place(recv);
        }
    }
}

/// The methods of `T[]` that leave the array unchanged.
const IN_PLACE_METHODS: [&str; 29] = [
    "at",
    "concat",
    "entries",
    "every",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "reduce",
    "reduceRight",
    "slice",
    "some",
    "toLocaleString",
    "toReversed",
    "toSorted",
    "toSpliced",
    "toString",
    "values",
    "with",
];

impl Ctx<'_> {
    /// An adapter of `iface` made at `span` reads slot `slot` (a field or a method's result)
    /// as a new array.
    pub(crate) fn note_adapted_array(&mut self, iface: DefId, slot: u32, span: Span) {
        let adapted = &mut self.iface_writes.array_reads.adapted;
        adapted.entry((iface, slot)).or_insert(span);
    }

    /// Once every body is checked and every adapter made: reports each read of an adapted
    /// array field that is not used in place.
    pub(crate) fn check_adapted_array_reads(&mut self) {
        let r = std::mem::take(&mut self.iface_writes.array_reads);
        let mut found: Vec<_> = (r.reads.into_iter())
            .filter(|(_, read)| !read.in_place)
            .filter_map(|(span, read)| {
                let at = *r.adapted.get(&(read.iface, read.slot))?;
                Some((span, read, at))
            })
            .collect();
        found.sort_by_key(|(s, ..)| (s.file, s.lo, s.hi));
        for (span, read, at) in found {
            let (what, t) = (&read.what, &read.through);
            let iname = self.iface_name(read.iface);
            self.error(
                Diagnostic::error(
                    format!("{what} through `{t}` may be a new array of converted values, and this use would not reach the array it comes from: not supported yet (#1185)"),
                    span,
                )
                .with_label(at, format!("a value implementing `{iname}` at other type arguments is converted here"))
                .with_note(format!("through `{t}`, use the array only in place: an element, `.length`, `for…of` over its elements, or a method that does not change it (`map`, `slice`, `indexOf`, …)"))
                .with_note("change, keep or compare it through the value's own type instead"),
            );
        }
    }
}
