//! std's function-type throws sites in a check by units (`VELT_UNITS=2`): the traps of
//! std/lang/proxy's `ProxyHandler` ([`Ctx::site_module`]). std's unit resolves their types before
//! any program stores into them; a whole-program check resolves them from its seeds once the
//! program's traps throw. So std's unit keeps the sites (`units::Summaries::trap_sites`), and
//! the app's rounds hold a trap closure to what their seeds give its site.

use rustc_hash::FxHashSet;
use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::ThrowSrc;
use crate::hir::{DefId, TyId, TyKind, TyOp};

impl Ctx<'_> {
    /// std's sites with the types std's check resolved them to: every function-type throws
    /// site std's check recorded (`record_fn_site`), whatever module of std it is in, so a new
    /// one is kept for the app's rounds too.
    pub(crate) fn std_trap_sites(&self) -> Vec<(Span, TyId)> {
        let std = &self.fn_sites.found.proxy_sites;
        let mut out: Vec<(Span, TyId)> = (self.fn_sites.sites.iter())
            .filter(|(s, _)| std.contains(s))
            .copied()
            .collect();
        out.sort_by_key(|(s, t)| (s.file.0, s.lo, t.0));
        out
    }

    /// In the app's stage of a check by units, std's functions whose error type grows with what
    /// the program's traps throw: those calling a trap (a source throwing a trap's error type,
    /// [`Self::trap_call_error`]), and the std functions calling them. A whole-program check
    /// infers them with the program's traps in the sites; std's unit inferred them before any
    /// program existed, with the sites empty. So the app's fixpoint takes them in again and
    /// raises their error types (`units::raised`).
    pub(crate) fn std_trap_callers(&self) -> Vec<DefId> {
        if self.units_stage != crate::units::Stage::App || self.unit_sums.trap_sites.is_empty() {
            return vec![];
        }
        let std_fns: Vec<DefId> = (self.fn_defs.iter().copied())
            .filter(|d| d.0 < self.unit_base)
            .filter(|d| self.fn_info(*d).state == crate::defs::BodyState::Done)
            .collect();
        let mut out: FxHashSet<DefId> = (std_fns.iter().copied())
            .filter(|d| {
                (self.fn_info(*d).throw_srcs.iter()).any(|s| {
                    matches!(s, ThrowSrc::Direct(t, _) if self.trap_site_of_error(*t).is_some())
                })
            })
            .collect();
        loop {
            let more: Vec<DefId> = (std_fns.iter().copied())
                .filter(|d| !out.contains(d))
                .filter(|d| {
                    (self.fn_info(*d).throw_srcs.iter())
                        .any(|s| matches!(s, ThrowSrc::Call(g, _, _) if out.contains(g)))
                })
                .collect();
            if more.is_empty() {
                break;
            }
            out.extend(more);
        }
        let mut out: Vec<DefId> = out.into_iter().collect();
        out.sort();
        out
    }

    /// The std trap site whose function type throws `t` (std's check gave each trap's type its
    /// own error type, which a call of the trap throws: `ThrowSrc::Direct`).
    fn trap_site_of_error(&self, t: TyId) -> Option<Span> {
        // A trap is an optional field (`get?: (…) => any`): its function type under `undefined`.
        let throws_t = |f: TyId| {
            let f = self.ty.core(f).unwrap_or(f);
            matches!(self.ty.kind(f), TyKind::FnPtr { throws, .. } if *throws == t)
        };
        (self.unit_sums.trap_sites.iter())
            .find(|(_, f)| throws_t(*f))
            .map(|(s, _)| *s)
    }

    /// In the app's stage of a check by units, what a std call of a trap throws, when `t` (what
    /// the call throws, `ThrowSrc::Direct`) is a std trap's error type: what the program's traps
    /// stored into its site throw ([`Self::app_trap_error`]), as a whole-program check has
    /// there. `None` for any other error type, which counts as it is.
    pub(crate) fn trap_call_error(&mut self, t: TyId) -> Option<Option<TyId>> {
        if self.units_stage != crate::units::Stage::App {
            return None;
        }
        let site = self.trap_site_of_error(t)?;
        if self.fn_sites.seeds.sites.contains_key(&site) {
            return Some(self.app_trap_error(Some(site)));
        }
        // No seeded set: the trap's variable is this round's, solved with the others
        // (`solve_err_vars`) when the check has variables; without them the site is empty.
        match self.err_var(t) {
            Some(v) if self.fn_sites.use_vars && self.fn_sites.trap_vars.contains(&v) => None,
            _ => Some(None),
        }
    }

    /// In a round of the app's stage without error-set variables (the sets seeded from the
    /// round before), std's trap variables, which std's frozen types still hold, stand for what
    /// the seeds give their sites ([`Self::app_trap_error`]), `never` for a site without a seed:
    /// their solutions, as `solve_err_vars` gives them in a round with variables, so the passes
    /// after inference and lowering see solved types. False when there is nothing to solve.
    pub(crate) fn seed_trap_vars(&mut self) -> bool {
        if self.units_stage != crate::units::Stage::App
            || self.fn_sites.use_vars
            || self.fn_sites.trap_vars.is_empty()
        {
            return false;
        }
        let mut vs: Vec<u32> = self.fn_sites.trap_vars.iter().copied().collect();
        vs.sort_unstable();
        for v in vs {
            let site = self.fn_sites.vars.get(v as usize).copied();
            let e = self.app_trap_error(site).unwrap_or(self.ty.never);
            let var = self.ty.intern(TyKind::Op(TyOp::ErrSite(v), vec![]));
            self.fn_sites.solved.insert(var, e);
        }
        true
    }

    /// In the app's stage of a check by units, the error type std trap site `site` has in this
    /// round: what this round's seeds give it, as a whole-program check resolves the trap's
    /// type from them. `None` elsewhere, and for a site that is no std trap's.
    pub(crate) fn app_trap_error(&mut self, site: Option<Span>) -> Option<TyId> {
        let span = site?;
        if self.units_stage != crate::units::Stage::App
            || !(self.unit_sums.trap_sites.iter()).any(|(s, _)| *s == span)
        {
            return None;
        }
        let members = self.fn_sites.seeds.sites.get(&span)?.clone();
        let ms: Vec<TyId> = members.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::TyKind;
    use crate::throws::fn_types::Portable;
    use velt_common::FileId;

    /// Every site std's check records is kept for the app's rounds, and only std's: a new std
    /// throws site is covered without a list to keep up to date. In the app's stage a kept site
    /// gives what the round's seeds hold for it; outside it, or for the program's own sites,
    /// nothing.
    #[test]
    fn every_std_throws_site_is_kept_and_takes_the_app_s_seeds() {
        let mut cx = Ctx::new(&[], 0);
        let at = |lo: u32| Span::new(FileId(1), lo, lo + 10);
        let (trap, other_trap, program) = (at(0), at(100), at(200));
        let ty = cx.ty.str_;
        cx.fn_sites.sites = vec![(program, ty), (other_trap, ty), (trap, ty)];
        cx.fn_sites.found.proxy_sites.insert(trap);
        cx.fn_sites.found.proxy_sites.insert(other_trap);
        assert_eq!(cx.std_trap_sites(), vec![(trap, ty), (other_trap, ty)]);

        // std's stage ends as a check by units ends it, keeping its sites for the app's rounds.
        crate::units::finish_std(&mut cx);
        assert_eq!(cx.unit_sums.trap_sites, vec![(trap, ty), (other_trap, ty)]);
        let thrown = Portable::Leaf(TyKind::Str);
        cx.fn_sites.seeds.sites.insert(trap, vec![thrown.clone()]);
        cx.fn_sites.seeds.sites.insert(program, vec![thrown]);
        assert_eq!(
            cx.app_trap_error(Some(trap)),
            None,
            "only in the app's stage"
        );
        cx.units_stage = crate::units::Stage::App;
        let want = cx.error_union(&[ty]);
        assert_eq!(cx.app_trap_error(Some(trap)), want);
        assert_eq!(cx.app_trap_error(Some(other_trap)), None, "not seeded");
        assert_eq!(
            cx.app_trap_error(Some(program)),
            None,
            "the program's own site"
        );
        assert_eq!(cx.app_trap_error(None), None);
    }
}
