//! Which types hold a promise (`Ctx::holds_promise`): the `Mutex` checks
//! (`ownership/locked`) look for promises a `with` callback makes from the locked value, which
//! would run after the lock is released.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};
use crate::type_reach::Look;

impl Ctx<'_> {
    /// Does a value of `t` hold a promise (itself, or in a part, within 8 levels)?
    pub(crate) fn holds_promise(&mut self, t: TyId) -> bool {
        let mut look = |cx: &mut Self, t: TyId| {
            let parts: Vec<TyId> = match cx.ty.kind(t).clone() {
                TyKind::Promise(..) => return Look::Found,
                TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
                TyKind::Tuple(ts) => ts,
                TyKind::Adt(d, args) => {
                    let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                        DefInfo::Enum(e) => {
                            e.variants.iter().flat_map(|v| v.payload.clone()).collect()
                        }
                        _ => vec![],
                    };
                    tys.into_iter().map(|f| cx.subst(f, &args)).collect()
                }
                _ => vec![],
            };
            Look::Parts(parts)
        };
        self.find_within(t, 8, &mut look).is_some()
    }
}
