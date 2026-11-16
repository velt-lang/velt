//! `Ptr<T>`, velt/ffi's C pointer (#3094): a type kind of its own (`TyKind::Ptr`), not the std
//! declaration that gives it its name, documentation and statics (`std/ffi/ptr.velt`, found
//! through its `std_hooks` entry `ffi_ptr_type`, never by name). Every type written with that
//! declaration's name is the kind; the declaration itself is never a type.

use velt_common::std_hook;
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, TyId, TyKind};

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

    /// Is `t` a C pointer, `Ptr<T>`?
    pub(crate) fn is_ptr_ty(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Ptr(_))
    }
}
