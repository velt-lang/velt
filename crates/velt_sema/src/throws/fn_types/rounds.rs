//! The sites of a check by units from one round of the app's stage to the next.

use super::*;

impl FnSites {
    /// These sites for another round of a check by units (`crate::units`), which starts from
    /// std's frozen unit: what std's check recorded (where types are written, std's sites and
    /// flows) stays; what depends on the round's sets starts as [`Self::new`] starts it.
    pub fn reseed(&mut self, seeds: FnTypeSeeds, last: bool) {
        let fresh = FnSites::new(seeds, last);
        self.use_vars = fresh.use_vars;
        self.user_vars = fresh.user_vars;
        self.found = fresh.found;
        self.seeds = fresh.seeds;
        self.last = last;
        self.grew = false;
        self.sites_grew = false;
        self.quiet.clear();
        self.indirect.clear();
        self.indirect_sites.clear();
        // What the variables solved to is this round's: a type solved in an earlier round (a
        // union of trap variables, `never` while the traps' sets were empty) is solved again
        // from this round's sets.
        self.solved.clear();
        self.sets_empty = None;
    }
}
