//! The class hierarchy as the checking context sees it.

use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId, TyId, TyKind};

impl Ctx<'_> {
    /// Is `t` a class instance type? Returns (class def, type args).
    pub fn class_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Base class type of class instance type `t` (with `t`'s type args substituted).
    pub fn base_of(&mut self, t: TyId) -> Option<TyId> {
        let (d, args) = self.class_of(t)?;
        let base = self.adt(d)?.base?;
        Some(self.subst(base, &args))
    }

    /// Is class `sub` class `sup` or one of its (transitive) subclasses?
    pub fn class_extends(&self, sub: DefId, sup: DefId) -> bool {
        let mut cur = Some(sub);
        for _ in 0..64 {
            match cur {
                Some(d) if d == sup => return true,
                Some(d) => {
                    cur = self
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.class_of(b))
                        .map(|(d, _)| d)
                }
                None => return false,
            }
        }
        false
    }
}
