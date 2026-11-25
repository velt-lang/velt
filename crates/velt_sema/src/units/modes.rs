//! The pass modes of a finished unit's functions and interface methods (`VELT_UNITS=2`).
//! Ownership inference raises a receiver's or a parameter's mode (`Borrow` to `BorrowMut` or
//! `Owned`, `ownership::mutation`), a function's and an interface slot's (the join over its
//! implementations), once every body is checked. A whole-program check therefore checks the
//! program's bodies with std's modes as declared, and only the passes after the bodies judge what
//! a call of std changes. std's unit keeps the modes its inference raised, and beside them the
//! declared modes of what it raised (`Summaries::declared_modes`), which the app's body check
//! reads while its bodies are checked ([`Ctx::declared_this`], [`Ctx::declared_slot`]). Nothing in std's
//! frozen unit is written.

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, PassMode};

/// The receiver and parameter modes of a function (`slot` none) or of slot `slot` of an
/// interface (its receiver `BorrowMut` when a call may change it, `IfaceMethod::mut_this`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Modes {
    def: DefId,
    slot: Option<u32>,
    this: Option<PassMode>,
    params: Vec<PassMode>,
}

/// The modes of definition `def`: a function's, or each of an interface's slots.
fn of(cx: &Ctx, def: DefId) -> Vec<Modes> {
    match cx.info.get(def.0 as usize) {
        Some(DefInfo::Fn(f)) => vec![Modes {
            def,
            slot: None,
            this: f.this.as_ref().map(|t| t.mode),
            params: f.params.iter().map(|p| p.mode).collect(),
        }],
        Some(DefInfo::Iface(i)) => (i.methods.iter().enumerate())
            .map(|(k, m)| Modes {
                def,
                slot: Some(k as u32),
                this: Some(match m.mut_this {
                    true => PassMode::BorrowMut,
                    false => PassMode::Borrow,
                }),
                params: m.params.iter().map(|p| p.mode).collect(),
            })
            .collect(),
        _ => vec![],
    }
}

/// std's stage, after its bodies: every function's and interface slot's modes as declared,
/// before its inference (`Summaries::declared_modes` holds them until [`keep_declared`]).
pub(crate) fn declared(cx: &mut Ctx) {
    let all = (0..cx.info.len() as u32)
        .flat_map(|i| of(cx, DefId(i)))
        .collect();
    cx.unit_sums.declared_modes = all;
}

/// End of std's stage: the declared modes of what its inference raised, sorted for the app's
/// lookups; the unit itself keeps the raised modes.
pub(crate) fn keep_declared(cx: &mut Ctx) {
    let declared = std::mem::take(&mut cx.unit_sums.declared_modes);
    let mut kept: Vec<Modes> = (declared.into_iter())
        .filter(|d| (of(cx, d.def).iter()).any(|now| now.slot == d.slot && now != d))
        .collect();
    kept.sort_by_key(|m| (m.def, m.slot));
    cx.unit_sums.declared_modes = kept;
    cx.unit_sums.declared_now = false;
}

impl Ctx<'_> {
    /// The declared modes of std's `def` (slot `slot` of an interface) while the app's bodies
    /// are checked, when std's inference raised them; `None` otherwise.
    fn declared_of(&self, def: DefId, slot: Option<u32>) -> Option<&Modes> {
        if !self.unit_sums.declared_now || def.0 >= self.unit_base {
            return None;
        }
        let list = &self.unit_sums.declared_modes;
        let i = list
            .binary_search_by_key(&(def, slot), |m| (m.def, m.slot))
            .ok()?;
        list.get(i)
    }

    /// Function `def`'s receiver mode as declared, while the app's bodies are checked.
    pub(crate) fn declared_this(&self, def: DefId) -> Option<PassMode> {
        self.declared_of(def, None)?.this
    }

    /// Slot `slot` of interface `iface` as declared (whether a call may change the receiver, and
    /// the parameter modes), while the app's bodies are checked.
    pub(crate) fn declared_slot(&self, iface: DefId, slot: u32) -> Option<(bool, &[PassMode])> {
        let m = self.declared_of(iface, Some(slot))?;
        Some((m.this == Some(PassMode::BorrowMut), &m.params))
    }
}

// The encoding of a finished unit (`codec`): every field, so a new one fails to build.
super::codec::structs! {
    Modes {
        def, slot, this, params,
    }
}
