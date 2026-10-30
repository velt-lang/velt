//! Hints between checks: what one check inferred a recursive function, or a call through an
//! interface slot without a `throws` clause, throws, kept for the next check
//! ([`FnTypeSeeds::hints`], [`FnTypeSeeds::slots`]), where a call whose callee is still being
//! checked counts it.

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId};

use super::fn_types::Portable;

impl Ctx<'_> {
    /// Keep the error types this check inferred for the next check, when there is one.
    pub(crate) fn record_recursion_hints(&mut self, fns: &[DefId]) {
        if !self.fn_sites.again() {
            return;
        }
        for &d in fns {
            if d.0 as usize >= self.collected_defs {
                continue;
            }
            let Some(t) = self.fn_info(d).throws else {
                continue;
            };
            let ms: Option<Vec<Portable>> = (self.error_members(t).into_iter())
                .map(|m| self.port(m, 0))
                .collect();
            if let Some(ms) = ms {
                let span = self.def_spans[d.0 as usize];
                self.fn_sites.found.hints.insert(d, (span, ms));
            }
        }
    }

    /// A type built while a body was checked (a generator's or a function value's, a `catch`
    /// variable's) missed what a function in a recursion throws, because that function's body
    /// was still being checked. Keep what the functions `srcs` call (through any depth of calls)
    /// throw in the final inference for the next check, where a call to one whose body is
    /// still being checked counts it ([`Ctx::recursion_hint`]), as TypeScript has no error
    /// types to annotate. True when the program is checked again for it.
    pub(crate) fn recursion_found_late(&mut self, srcs: &[crate::defs::ThrowSrc]) -> bool {
        if self.fn_sites.last {
            return false;
        }
        let mut grew = false;
        let mut todo = srcs.to_vec();
        let mut seen: Vec<DefId> = vec![];
        while let Some(s) = todo.pop() {
            let crate::defs::ThrowSrc::Call(d, ..) = s else {
                continue;
            };
            if d.0 as usize >= self.collected_defs || seen.contains(&d) {
                continue;
            }
            seen.push(d);
            let Some(f) = self.try_fn(d) else {
                continue;
            };
            if f.declared_throws.is_some_and(|t| !t.from_body) {
                continue;
            }
            todo.extend(f.throw_srcs.iter().cloned());
            if let Some(t) = f.throws {
                grew |= self.add_recursion_hint(d, t);
            }
        }
        if grew {
            self.fn_sites.grew = true;
        }
        grew
    }

    /// Add the members of `t` to the hint of function `d` for the next check; true when new.
    fn add_recursion_hint(&mut self, d: DefId, t: TyId) -> bool {
        let ms: Option<Vec<Portable>> = (self.error_members(t).into_iter())
            .map(|m| self.port(m, 0))
            .collect();
        let Some(ms) = ms else {
            return false;
        };
        let span = self.def_spans[d.0 as usize];
        let mut all = match self.fn_sites.found.hints.get(&d) {
            Some((s, old)) if *s == span => old.clone(),
            _ => vec![],
        };
        let before = all.len();
        for m in ms {
            if !all.contains(&m) {
                all.push(m);
            }
        }
        let new = all.len() > before;
        self.fn_sites.found.hints.insert(d, (span, all));
        new
    }

    /// What the previous check inferred function `d` throws (see [`FnTypeSeeds::hints`]).
    pub(crate) fn recursion_hint(&mut self, d: DefId) -> Option<TyId> {
        let (span, ms) = self.fn_sites.seeds.hints.get(&d)?.clone();
        if self.def_spans.get(d.0 as usize) != Some(&span) {
            return None;
        }
        let ms: Vec<TyId> = ms.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }

    /// Keep `t`, what slot `slot` of `iface` throws in the final inference, for the next check
    /// (see [`FnTypeSeeds::slots`]); true when the program is checked again for it.
    pub(crate) fn record_slot_hint(&mut self, iface: DefId, slot: u32, t: TyId) -> bool {
        if self.fn_sites.last || iface.0 as usize >= self.collected_defs {
            return false;
        }
        let ms: Option<Vec<Portable>> = (self.error_members(t).into_iter())
            .map(|m| self.port(m, 0))
            .collect();
        let Some(ms) = ms else {
            return false;
        };
        let span = self.def_spans[iface.0 as usize];
        let mut all = match self.fn_sites.found.slots.get(&(iface, slot)) {
            Some((s, old)) if *s == span => old.clone(),
            _ => vec![],
        };
        let before = all.len();
        for m in ms {
            if !all.contains(&m) {
                all.push(m);
            }
        }
        if all.len() == before {
            return false;
        }
        self.fn_sites.found.slots.insert((iface, slot), (span, all));
        self.fn_sites.grew = true;
        true
    }

    /// What the previous check inferred a call through slot `slot` of `iface` throws (see
    /// [`FnTypeSeeds::slots`]).
    pub(crate) fn slot_hint(&mut self, iface: DefId, slot: u32) -> Option<TyId> {
        let (span, ms) = self.fn_sites.seeds.slots.get(&(iface, slot))?.clone();
        if self.def_spans.get(iface.0 as usize) != Some(&span) {
            return None;
        }
        let ms: Vec<TyId> = ms.iter().filter_map(|p| self.unport(p)).collect();
        self.error_union(&ms)
    }
}
