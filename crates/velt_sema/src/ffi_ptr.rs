//! `Ptr<T>`, velt/ffi's C pointer (#3094): a type kind of its own (`TyKind::Ptr`), not the std
//! declaration that gives it its name, documentation and statics (`std/ffi/ptr.velt`, found
//! through its `std_hooks` entry `ffi_ptr_type`, never by name). Every type written with that
//! declaration's name is the kind; the declaration itself is never a type.

use velt_common::{std_hook, Diagnostic};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::{Ctx, Item};
use crate::hir::{self, DefId, TyId, TyKind};

impl Ctx<'_> {
    /// Is `d` std's declaration of `Ptr` (`std/ffi/ptr.velt`)?
    pub(crate) fn is_ptr_decl(&self, d: DefId) -> bool {
        self.std_def(std_hook!(ffi_ptr_type)) == Some(Item::Def(d))
    }

    /// The type `Ptr<args…>` written at `t`: the pointer kind of its one argument, which is
    /// what C can point at where it is known (a type parameter is checked in the C signature
    /// or C struct it reaches, `collect::c_sig`).
    pub(crate) fn ptr_type_written(&mut self, args: &[TyId], t: &ast::TypeExpr) -> TyId {
        let [pointee] = args else {
            self.arity_error("Ptr", 1, args.len(), t);
            return self.ty.error;
        };
        if !self.c_pointee_written(*pointee) {
            let shown = self.display(*pointee);
            self.error(
                Diagnostic::error(format!("a C pointer can't point at `{shown}`"), t.span)
                    .with_note("`Ptr<T>` points at a number, `bool`, `void`, a `declare c type`, a `declare c struct` or another pointer: C has no other values"),
            );
            return self.ty.error;
        }
        self.ty.intern(TyKind::Ptr(*pointee))
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
    /// any depth (#3094)? Text of one would show the pointer (JS writes `${[p]}` and
    /// `[p].join()` element by element), and one converted from `any` would read the pointer
    /// out of a dynamic value (an array view reads each element through `any`). A class or a map
    /// holding one is no such type: its text is `[object Object]`, `[object Map]` or its own
    /// `toString()`, and from `any` it is the object the `any` holds.
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
    pub(crate) fn check_instance_pointee(&mut self, pointee: TyId, span: velt_common::Span) {
        if crate::collect::c_sig::c_pointee(self, pointee) {
            return;
        }
        let shown = self.display(pointee);
        self.error(
            Diagnostic::error(format!("a C pointer can't point at `{shown}`"), span)
                .with_note("a type parameter used as `Ptr<T>`'s pointee is instantiated here: `Ptr<T>` points at a number, `bool`, `void`, a `declare c type`, a `declare c struct` or another pointer"),
        );
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
        if !self.cx.ptr_or_element(to) {
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
