//! Which types hold a promise (`Ctx::holds_promise`): promises have one owner, so values that
//! hold one are moved rather than shared.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// Does a value of `t` hold a promise (itself, or in a part)?
    pub(crate) fn holds_promise(&mut self, t: TyId) -> bool {
        self.holds_promise_in(t, 0)
    }

    pub(crate) fn holds_promise_in(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Promise(..) => return true,
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Tuple(ts) => ts,
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.subst(f, &args)).collect()
            }
            _ => vec![],
        };
        parts
            .into_iter()
            .any(|p| self.holds_promise_in(p, depth + 1))
    }
}
