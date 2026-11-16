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

    /// The type `Ptr<args…>` written at `t`: the pointer kind of its one argument.
    pub(crate) fn ptr_type_written(&mut self, args: &[TyId], t: &ast::TypeExpr) -> TyId {
        match args {
            [pointee] => self.ty.intern(TyKind::Ptr(*pointee)),
            _ => {
                self.arity_error("Ptr", 1, args.len(), t);
                self.ty.error
            }
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
}

impl FnCx<'_, '_> {
    /// Reports a C pointer converted to text (a template literal part, an operand of `+` with a
    /// string, `String(p)`, `typeof p`): a pointer has none (#3094). True if `h` is one.
    pub(crate) fn reject_ptr_text(&mut self, h: &hir::Expr, what: &str) -> bool {
        if !self.cx.is_ptr_value(h.ty) {
            return false;
        }
        let shown = self.cx.display(h.ty);
        self.cx.error(
            Diagnostic::error(format!("a C pointer (`{shown}`) has no {what}"), h.span).with_note(
                "`Ptr.addressOf(p)` gives its address, a number; `console.log(p)` shows it",
            ),
        );
        true
    }

    /// Reports a C pointer converted to `any`, `unknown`, `object` or an interface: it has no
    /// dynamic form (#3094). A type holding one converts. True if `h` is one.
    pub(crate) fn reject_ptr_dynamic(&mut self, h: &hir::Expr, to: TyId) -> bool {
        if !self.cx.is_ptr_value(h.ty) {
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
}
