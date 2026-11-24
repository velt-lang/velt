//! What the app's passes change of a finished unit's definitions (#3543), kept beside them:
//! the app's code can raise the pass modes of a std method it overrides or of a std generated
//! function it instantiates (`ownership::mutation`'s groups), the modes of a std interface's slot
//! it implements, and the throws of a std function whose throw group it joins
//! (`throws::infer`). In a whole-program check those write the definitions; in the app's stage
//! of a check by units std's definitions are frozen, so the raised values are kept here, read in
//! their place while the app's passes run ([`Ctx::raised_modes_of`], [`Ctx::slot_modes`],
//! [`Ctx::fn_throws`]), and written onto the definitions once the freeze lifts to build the
//! program ([`apply`]), which a whole-program check had written by then.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, PassMode, TyId};

/// The raised values of a finished unit's definitions (empty in a saved unit).
#[derive(Clone, Default)]
pub struct Raised {
    /// A function's modes, `this` first (as `ownership::patch::modes_of` gives them).
    pub(crate) fns: FxHashMap<DefId, Vec<PassMode>>,
    /// Interface slot `(iface, slot)`: whether a call may change the receiver, and the
    /// parameters' modes.
    pub(crate) slots: FxHashMap<(DefId, u32), (bool, Vec<PassMode>)>,
    /// A function's final throws.
    pub(crate) throws: FxHashMap<DefId, Option<TyId>>,
    /// Functions used as values whose raised parameters were demoted back to `Borrow`
    /// (`ownership::finish::demote_fn_values`): the parameter indices (`this` not counted) whose
    /// locals become `mutable` in the body.
    pub(crate) demoted: FxHashMap<DefId, Vec<usize>>,
}

/// A saved unit holds none: what a finished unit keeps is raised only by a later unit's passes,
/// and [`apply`] empties it before a unit is saved.
impl super::codec::Codec for Raised {
    fn enc(&self, _: &mut super::codec::Writer) {
        assert!(
            self.fns.is_empty()
                && self.slots.is_empty()
                && self.throws.is_empty()
                && self.demoted.is_empty(),
            "ICE: a unit saved with raised definitions"
        );
    }

    fn dec(_: &mut super::codec::Reader) -> Self {
        Raised::default()
    }
}

impl Ctx<'_> {
    /// Function `d`'s modes as the app's passes raised them, when `d` is a finished unit's.
    pub(crate) fn raised_modes_of(&self, d: DefId) -> Option<&Vec<PassMode>> {
        self.unit_sums.raised.fns.get(&d)
    }

    /// Function `d`'s parameter modes (`this` not included) as its signature has them, raised as
    /// the app's passes raised them when `d` is a finished unit's.
    pub(crate) fn own_param_modes(&self, d: DefId) -> Vec<PassMode> {
        let info = self.fn_info(d);
        match self.raised_modes_of(d) {
            Some(m) => m[usize::from(info.this.is_some())..].to_vec(),
            None => info.params.iter().map(|p| p.mode).collect(),
        }
    }

    /// Interface `iface`'s slot `slot`: whether a call may change the receiver, and the
    /// parameters' modes, as raised in the app's passes when the interface is a finished unit's.
    pub(crate) fn slot_modes(&self, iface: DefId, slot: usize) -> Option<(bool, Vec<PassMode>)> {
        if let Some(r) = self.unit_sums.raised.slots.get(&(iface, slot as u32)) {
            return Some(r.clone());
        }
        let m = self.iface(iface)?.methods.get(slot)?;
        Some((m.mut_this, m.params.iter().map(|p| p.mode).collect()))
    }

    /// Function `d`'s final throws, as raised in the app's passes when `d` is a finished unit's.
    pub(crate) fn fn_throws(&self, d: DefId) -> Option<TyId> {
        match self.unit_sums.raised.throws.get(&d) {
            Some(t) => *t,
            None => self.fn_info(d).throws,
        }
    }

    /// Function `d` takes modes `modes` (`this` first): written, or kept when `d` is frozen.
    pub(crate) fn raise_modes(&mut self, d: DefId, modes: Vec<PassMode>) {
        if self.is_frozen(d) {
            self.unit_sums.raised.fns.insert(d, modes);
            return;
        }
        write_modes(self, d, &modes);
    }

    /// Interface slot `(iface, slot)` takes `mut_this` and `params`: written, or kept when the
    /// interface is frozen.
    pub(crate) fn raise_slot(
        &mut self,
        iface: DefId,
        slot: usize,
        mut_this: bool,
        params: Vec<PassMode>,
    ) {
        if self.is_frozen(iface) {
            self.unit_sums
                .raised
                .slots
                .insert((iface, slot as u32), (mut_this, params));
            return;
        }
        write_slot(self, iface, slot, mut_this, &params);
    }

    /// Function `d`'s final throws are `t`: written, or kept when `d` is frozen.
    pub(crate) fn raise_throws(&mut self, d: DefId, t: Option<TyId>) {
        if self.is_frozen(d) {
            self.unit_sums.raised.throws.insert(d, t);
            return;
        }
        self.fn_info_mut(d).throws = t;
    }
}

fn write_modes(cx: &mut Ctx, d: DefId, modes: &[PassMode]) {
    let f = cx.fn_info_mut(d);
    let mut it = modes.iter().copied();
    if let Some(t) = f.this.as_mut() {
        t.mode = it.next().expect("ICE: raised this");
    }
    for (p, m) in f.params.iter_mut().zip(it) {
        p.mode = m;
    }
}

fn write_slot(cx: &mut Ctx, iface: DefId, slot: usize, mut_this: bool, params: &[PassMode]) {
    let DefInfo::Iface(info) = &mut cx.info[iface.0 as usize] else {
        unreachable!("ICE: iface")
    };
    let m = &mut info.methods[slot];
    m.mut_this = mut_this;
    for (p, j) in m.params.iter_mut().zip(params) {
        p.mode = *j;
    }
}

/// Once the freeze lifts to build the program (`analysis`): write what the app's passes kept,
/// as a whole-program check had written it.
pub(crate) fn apply(cx: &mut Ctx) {
    let raised = std::mem::take(&mut cx.unit_sums.raised);
    let mut fns: Vec<_> = raised.fns.into_iter().collect();
    fns.sort_by_key(|(d, _)| *d);
    // `VELT_UNITS_RAISED=1`: what the program raised of a finished unit's definitions, one line
    // each, as the `velt-units:` report lists the writes (the tests read it).
    if std::env::var_os("VELT_UNITS_RAISED").is_some() {
        for (d, _) in &fns {
            eprintln!("velt-units-raised: modes {}", cx.fn_info(*d).name);
        }
        for (iface, slot) in raised.slots.keys() {
            let name = cx.iface(*iface).and_then(|i| i.methods.get(*slot as usize));
            eprintln!(
                "velt-units-raised: slot {}",
                name.map_or("?", |m| m.name.as_str())
            );
        }
        for d in raised.throws.keys() {
            eprintln!("velt-units-raised: throws {}", cx.fn_info(*d).name);
        }
    }
    for (d, modes) in fns {
        write_modes(cx, d, &modes);
        // The body's parameters too, which lowering reads, as `ownership::infer`'s
        // `sync_params` gives a checked unit's functions theirs.
        if let Some(crate::hir::Def::Fn(f)) = &mut cx.defs[d.0 as usize] {
            let skip = f.params.len().saturating_sub(modes.len());
            for (p, m) in f.params.iter_mut().skip(skip).zip(&modes) {
                p.mode = *m;
            }
        }
    }
    for (d, ks) in raised.demoted {
        let skip = usize::from(cx.fn_info(d).this.is_some());
        if let Some(crate::hir::Def::Fn(f)) = &mut cx.defs[d.0 as usize] {
            for k in ks {
                if let Some(l) = f.params.get(skip + k).map(|p| p.local) {
                    f.body.locals[l.0 as usize].mutable = true;
                }
            }
        }
    }
    let mut slots: Vec<_> = raised.slots.into_iter().collect();
    slots.sort_by_key(|(k, _)| *k);
    for ((iface, slot), (mut_this, params)) in slots {
        write_slot(cx, iface, slot as usize, mut_this, &params);
    }
    for (d, t) in raised.throws {
        cx.fn_info_mut(d).throws = t;
    }
}
