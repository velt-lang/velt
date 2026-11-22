//! The pass modes of a finished unit's functions and interface methods (`VELT_UNITS=2`).
//! Ownership inference raises a receiver's or a parameter's mode (`Borrow` to `BorrowMut` or
//! `Owned`, `ownership::mutation`), a function's and an interface slot's (the join over its
//! implementations), once every body is checked. A whole-program check therefore checks the
//! program's bodies with std's modes as declared, and only the passes after the bodies judge what
//! a call of std changes. std's unit keeps the modes as declared for the app's bodies, and the
//! modes its inference raised for the app's passes.

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

/// The modes `m` names now.
fn now(cx: &Ctx, m: &Modes) -> Option<Modes> {
    of(cx, m.def).into_iter().find(|n| n.slot == m.slot)
}

fn set(cx: &mut Ctx, m: &Modes) {
    let (this, params) = match (&mut cx.info[m.def.0 as usize], m.slot) {
        (DefInfo::Fn(f), None) => {
            if let (Some(t), Some(mode)) = (f.this.as_mut(), m.this) {
                t.mode = mode;
            }
            (None, &mut f.params)
        }
        (DefInfo::Iface(i), Some(k)) => {
            let Some(s) = i.methods.get_mut(k as usize) else {
                return;
            };
            (Some(&mut s.mut_this), &mut s.params)
        }
        _ => return,
    };
    if let (Some(mut_this), Some(mode)) = (this, m.this) {
        *mut_this = mode == PassMode::BorrowMut;
    }
    for (p, &mode) in params.iter_mut().zip(&m.params) {
        p.mode = mode;
    }
}

/// std's stage, after its bodies: every function's and interface slot's modes as declared,
/// before its inference (`Summaries::raised_modes` holds them until [`keep_raised`]).
pub(crate) fn declared(cx: &mut Ctx) {
    let all = (0..cx.info.len() as u32)
        .flat_map(|i| of(cx, DefId(i)))
        .collect();
    cx.unit_sums.raised_modes = all;
}

/// End of std's stage: what its inference raised gets its declared modes back, and the unit keeps
/// the raised ones for the app's passes ([`restore`]).
pub(crate) fn keep_raised(cx: &mut Ctx) {
    let declared = std::mem::take(&mut cx.unit_sums.raised_modes);
    let mut raised = vec![];
    for d in declared {
        match now(cx, &d) {
            Some(now) if now != d => {
                set(cx, &d);
                raised.push(now);
            }
            _ => {}
        }
    }
    cx.unit_sums.raised_modes = raised;
}

/// The app's stage, after its bodies: std's functions and interface slots take the modes its
/// inference raised.
pub(crate) fn restore(cx: &mut Ctx) {
    for m in cx.unit_sums.raised_modes.clone() {
        set(cx, &m);
    }
}

// The encoding of a finished unit (`codec`): every field, so a new one fails to build.
super::codec::structs! {
    Modes {
        def, slot, this, params,
    }
}
