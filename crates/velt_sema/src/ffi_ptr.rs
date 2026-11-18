//! `Ptr<T>`, velt/ffi's C pointer (#3094): a type kind of its own (`TyKind::Ptr`), not the std
//! declaration that gives it its name, documentation and statics (`std/ffi/ptr.velt`, found
//! through its `std_hooks` entry `ffi_ptr_type`, never by name). Every type written with that
//! declaration's name is the kind; the declaration itself is never a type.

use velt_common::{std_hook, Diagnostic};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::{Ctx, Item};
use crate::hir::{self, DefId, TyId, TyKind};

/// The note of a pointee refused where an alias is used or declared.
const ALIAS_NOTE: &str = "an alias's `Ptr<T>` is instantiated here: `Ptr<T>` points at a number, `bool`, `void`, a `declare c type`, a `declare c struct` or another pointer";

impl Ctx<'_> {
    /// Is `d` std's declaration of `Ptr` (`std/ffi/ptr.velt`)?
    pub(crate) fn is_ptr_decl(&self, d: DefId) -> bool {
        self.std_def(std_hook!(ffi_ptr_type)) == Some(Item::Def(d))
    }

    /// The type `Ptr<args…>` written at `t`: the pointer kind of its one argument, which is
    /// what C can point at where it is known (a type parameter is checked in the C signature
    /// or C struct it reaches, `collect::c_sig`). `chosen`: the pointee as written names a type
    /// parameter of the alias being expanded (or a name a conditional type bound), so the
    /// alias's use chose it and a refusal goes there (`type_level.ptr_site`); a pointee fixed
    /// in the alias's body is refused at the body. Refused, it is still the pointer type, so
    /// nothing else about the type is reported.
    pub(crate) fn ptr_type_written(
        &mut self,
        args: &[TyId],
        t: &ast::TypeExpr,
        chosen: bool,
    ) -> TyId {
        let [pointee] = args else {
            self.arity_error("Ptr", 1, args.len(), t);
            return self.ty.error;
        };
        if !self.c_pointee_written(*pointee) {
            let (at, note) = match self.type_level.ptr_site {
                Some(at) if chosen => (at, ALIAS_NOTE),
                _ => (t.span, "`Ptr<T>` points at a number, `bool`, `void`, a `declare c type`, a `declare c struct` or another pointer: C has no other values"),
            };
            self.refuse_pointee(*pointee, at, note);
        }
        self.ty.intern(TyKind::Ptr(*pointee))
    }

    /// Reports that C can't point at `pointee`, at `at`, unless that was reported already.
    fn refuse_pointee(&mut self, pointee: TyId, at: velt_common::Span, note: &'static str) {
        self.type_level.ptr_refusals += 1;
        let ptr = self.ty.intern(TyKind::Ptr(pointee));
        self.type_level.ptr_refused_types.insert(ptr);
        if !self.type_level.ptr_refused.insert(at) {
            return;
        }
        let shown = self.display(pointee);
        self.error(
            Diagnostic::error(format!("a C pointer can't point at `{shown}`"), at).with_note(note),
        );
    }

    /// Does `args`, the type arguments written to a `Ptr`, name a type parameter or a bound
    /// name of `env` (`ptr_type_written`'s `chosen`)?
    pub(crate) fn names_env(args: &[ast::TypeExpr], env: &crate::resolve::TyEnv) -> bool {
        let mut names = vec![];
        for a in args {
            crate::type_level::inline::names_in(a, &mut names);
        }
        names
            .iter()
            .any(|n| env.params.iter().any(|p| p == n) || env.locals.iter().any(|(l, _)| l == n))
    }

    /// Reports a `Ptr<T>` in `ty`, an alias expanded where `t` is written, that now points at
    /// what C has no values of: `type P<T> = Ptr<T>` written `P<string>`, or through another
    /// alias. Its body was checked with `T` unknown (`ptr_type_written`); a pointee that still
    /// mentions a type parameter is checked where that one is instantiated (`json`). The type
    /// stays as expanded, so nothing else about the use is reported for it.
    pub(crate) fn alias_pointees(&mut self, ty: TyId, at: velt_common::Span) {
        let mut todo = vec![(ty, 0u32)];
        let mut seen: rustc_hash::FxHashSet<TyId> = rustc_hash::FxHashSet::default();
        while let Some((u, depth)) = todo.pop() {
            if depth > crate::types::MAX_ARG_DEPTH || !seen.insert(u) {
                continue;
            }
            let kind = self.ty.kind(u).clone();
            if let TyKind::Ptr(p) = kind {
                if !mentions_param(&self.ty, p) && !self.c_pointee_written(p) {
                    return self.refuse_pointee(p, at, ALIAS_NOTE);
                }
            }
            todo.extend(
                crate::types::children(&kind)
                    .into_iter()
                    .map(|c| (c, depth + 1)),
            );
        }
    }

    /// Can C point at a `t`, as far as is known where a `Ptr<t>` is written? A type parameter,
    /// an error and a definition not collected yet pass; a type C has no values of doesn't.
    fn c_pointee_written(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Unit
            | TyKind::Ptr(_)
            | TyKind::Param(_)
            | TyKind::Error
            | TyKind::Op(..) => true,
            TyKind::Option(inner, nothing) => {
                // As `c_sig::c_pointee` (#3094): C has one NULL, not two.
                *nothing != hir::Nothing::Both && matches!(self.ty.kind(*inner), TyKind::Ptr(_))
            }
            TyKind::Adt(d, _) => self
                .adt(*d)
                .is_none_or(|a| a.decl.is_none_or(|t| t.c_type || t.c_struct)),
            _ => false,
        }
    }

    /// Is `t` a C pointer, `Ptr<T>` or `Ptr<T> | null`: a value with no dynamic form and no
    /// text? A type that holds one (a class with a pointer field) is not: it converts, its
    /// pointer unlisted (#3094).
    pub(crate) fn is_ptr_value(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Ptr(_) => true,
            TyKind::Option(inner, _) => matches!(self.ty.kind(*inner), TyKind::Ptr(_)),
            _ => false,
        }
    }

    /// Is `t` a C pointer, or an array, a tuple, an option or a union with one as an element, at
    /// any depth (#3094)? Text of one would show the pointer: JS writes `${[p]}` and
    /// `[p].join()` element by element. A class, a map or a record holding one is no such
    /// type: its text is `[object Object]`, `[object Map]` or its own `toString()`.
    pub(crate) fn ptr_or_element(&mut self, t: TyId) -> bool {
        self.ptr_or_element_at(t, 0)
    }

    fn ptr_or_element_at(&mut self, t: TyId, depth: u32) -> bool {
        if self.is_ptr_value(t) {
            return true;
        }
        if depth > crate::types::MAX_ARG_DEPTH {
            return false;
        }
        let parts = match self.ty.kind(t).clone() {
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Tuple(es) => es,
            _ => self.union_members(t).unwrap_or_default(),
        };
        parts
            .into_iter()
            .any(|p| self.ptr_or_element_at(p, depth + 1))
    }

    /// Would a view of an `any` as a `t` read a C pointer out of it (#3094): is `t` a pointer, or
    /// an array, tuple, option, union or `Record` with one as an element or value, at any depth?
    /// Each of these from `any` is a view reading its elements through `any` (array_view_cast,
    /// record_view). Classes, maps, sets and object types from `any` are checked at run time
    /// for the exact type the `any` holds, which a pointer is never part of.
    pub(crate) fn ptr_in_view(&mut self, t: TyId) -> bool {
        self.ptr_in_view_at(t, 0)
    }

    fn ptr_in_view_at(&mut self, t: TyId, depth: u32) -> bool {
        if self.is_ptr_value(t) {
            return true;
        }
        if depth > crate::types::MAX_ARG_DEPTH {
            return false;
        }
        let parts = match self.ty.kind(t).clone() {
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Tuple(es) => es,
            _ => match self.record_type_args(t) {
                Some((_, v)) => vec![v],
                None => self.union_members(t).unwrap_or_default(),
            },
        };
        parts.into_iter().any(|p| self.ptr_in_view_at(p, depth + 1))
    }

    /// Is `t` a C pointer, or a union with one among its members: a value whose dynamic form
    /// would be a pointer (#3094)?
    pub(crate) fn ptr_in_dynamic(&mut self, t: TyId) -> bool {
        self.is_ptr_value(t)
            || (self.union_members(t)).is_some_and(|ms| ms.iter().any(|&m| self.is_ptr_value(m)))
    }

    /// The pointees of the C pointer types in `t` that mention a type parameter: each is
    /// checked where the parameter is instantiated (`json`'s `Need::CPointee`, #3094).
    pub(crate) fn generic_pointees(tys: &crate::types::Types, t: TyId, out: &mut Vec<TyId>) {
        if let TyKind::Ptr(p) = tys.kind(t) {
            if mentions_param(tys, *p) {
                out.push(*p);
            }
        }
        for c in crate::types::children(tys.kind(t)) {
            Self::generic_pointees(tys, c, out);
        }
    }

    /// Reports an instance of a generic `Ptr<T>` whose `T` is no type C has values of (a
    /// `string`, a class): the check `ptr_type_written` leaves to the instantiation (#3094).
    /// True if it reported.
    pub(crate) fn check_instance_pointee(
        &mut self,
        pointee: TyId,
        span: velt_common::Span,
    ) -> bool {
        if crate::collect::c_sig::c_pointee(self, pointee) {
            return false;
        }
        let shown = self.display(pointee);
        self.error(
            Diagnostic::error(format!("a C pointer can't point at `{shown}`"), span)
                .with_note("a type parameter used as `Ptr<T>`'s pointee is instantiated here: `Ptr<T>` points at a number, `bool`, `void`, a `declare c type`, a `declare c struct` or another pointer"),
        );
        true
    }
}

/// What `ptrRead<T>` and `ptrWrite<T>` (velt/ffi's `Ptr.read`, `Ptr.write`) move.
pub(crate) const MEMORY_ELEMENTS: &str =
    "a number type, `bool`, a C struct or a C pointer (`Ptr<T>`, `Ptr<T> | null`)";

impl Ctx<'_> {
    /// Can `ptrRead<T>` / `ptrWrite<T>` (velt/ffi's `Ptr.read`, `Ptr.write`) move a `t`: a number
    /// type, `bool`, a C struct or a C pointer (`Ptr<U>`, `Ptr<U> | null`)? `void` and a
    /// `declare c type` have no size (#3094).
    pub(crate) fn memory_element(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => true,
            TyKind::Ptr(_) => true,
            TyKind::Option(inner, n) => {
                *n != crate::hir::Nothing::Both && matches!(self.ty.kind(*inner), TyKind::Ptr(_))
            }
            _ => self.is_c_struct_ty(t),
        }
    }

    /// Reports `Ptr.read<T>` / `Ptr.write<T>` instantiated with a `T` they can't move (generic
    /// code's `ptrRead<T>`, `ptrWrite<T>`: `json::PtrUse::Memory`). True if it reported.
    pub(crate) fn check_instance_memory(&mut self, t: TyId, span: velt_common::Span) -> bool {
        if self.memory_element(t) || matches!(self.ty.kind(t), TyKind::Error) {
            return false;
        }
        let shown = self.display(t);
        self.error(
            Diagnostic::error(
                format!("`Ptr.read` and `Ptr.write` can't move a `{shown}`"),
                span,
            )
            .with_note(format!(
                "a type parameter read or written through a pointer is instantiated here: it must be {}; `void` and a `declare c type` have no size",
                MEMORY_ELEMENTS
            )),
        );
        true
    }
}

impl FnCx<'_, '_> {
    /// Reports a C pointer converted to text (a template literal part, an operand of `+` with a
    /// string, `String(p)`, `xs.join()`), also as an element (`${[p]}`): a pointer has none
    /// (#3094). True if `h` is or holds one.
    pub(crate) fn reject_ptr_text(&mut self, h: &hir::Expr) -> bool {
        if !self.cx.ptr_or_element(h.ty) {
            return false;
        }
        let shown = self.cx.display(h.ty);
        let msg = match self.cx.is_ptr_value(h.ty) {
            true => format!("a C pointer (`{shown}`) has no text"),
            false => format!("a `{shown}` has no text: it holds a C pointer"),
        };
        self.cx.error(Diagnostic::error(msg, h.span).with_note(
            "`Ptr.addressOf(p)` gives a pointer's address, a number; `console.log(p)` shows it",
        ));
        true
    }

    /// Reports `typeof p` of a C pointer: it is no JavaScript value (#3094). True if `h` is one.
    pub(crate) fn reject_ptr_typeof(&mut self, h: &hir::Expr) -> bool {
        if !self.cx.ptr_in_dynamic(h.ty) {
            return false;
        }
        let shown = self.cx.display(h.ty);
        self.cx.error(
            Diagnostic::error(
                format!("a C pointer (`{shown}`) has no `typeof`: it is no JavaScript value"),
                h.span,
            )
            .with_note("`p === null` tells a NULL pointer from another"),
        );
        true
    }

    /// Reports a C pointer converted to `any`, `unknown`, `object` or an interface: it has no
    /// dynamic form (#3094). A type holding one converts. True if `h` is one.
    pub(crate) fn reject_ptr_dynamic(&mut self, h: &hir::Expr, to: TyId) -> bool {
        if !self.cx.ptr_in_dynamic(h.ty) {
            return false;
        }
        let (shown, target) = (self.cx.display(h.ty), self.cx.display(to));
        self.cx.error(
            Diagnostic::error(
                format!("a C pointer (`{shown}`) can't be converted to `{target}`"),
                h.span,
            )
            .with_note("a pointer has no dynamic form: keep it typed, or pass `Ptr.addressOf(p)`, its address"),
        );
        true
    }

    /// Reports an `any` or `unknown` converted to a C pointer (`a as Ptr<T>`), also as an
    /// element (`a as Ptr<T>[]`): no dynamic value holds one (#3094). True if `to` is one.
    pub(crate) fn reject_ptr_from_dynamic(&mut self, h: &hir::Expr, to: TyId) -> bool {
        if !self.cx.ptr_in_view(to) {
            return false;
        }
        let (shown, target) = (self.cx.display(h.ty), self.cx.display(to));
        let msg = match self.cx.is_ptr_value(to) {
            true => format!("an `{shown}` value can't be converted to a C pointer (`{target}`)"),
            false => format!(
                "an `{shown}` value can't be converted to a `{target}`: it holds a C pointer"
            ),
        };
        self.cx.error(
            Diagnostic::error(msg, h.span)
            .with_note("no dynamic value holds a pointer: make one with `Ptr.at(address)` or `Ptr.orNull(address)` in `unsafe` code"),
        );
        true
    }
}

/// Does `t` mention a type parameter?
fn mentions_param(tys: &crate::types::Types, t: TyId) -> bool {
    match tys.kind(t) {
        TyKind::Param(_) => true,
        k => crate::types::children(k)
            .into_iter()
            .any(|c| mentions_param(tys, c)),
    }
}
