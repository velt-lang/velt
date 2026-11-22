//! std's function-type throws sites in a check by units (`VELT_UNITS=2`): the traps of
//! std/lang/proxy's `ProxyHandler` ([`Ctx::site_module`]). std's unit resolves their types before
//! any program stores into them; a whole-program check resolves them from its seeds once the
//! program's traps throw. So std's unit keeps the sites (`units::Summaries::trap_sites`), and
//! the app's rounds hold a trap closure to what their seeds give its site.

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::TyId;

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
