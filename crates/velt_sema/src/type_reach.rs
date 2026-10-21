//! Breadth-first searches of the type graph to a given number of levels (`known::owns_resource`,
//! `known::uncopyable_part`, `holds_promise`): is a type of some kind within `levels` fields,
//! elements or payloads of `t`?
//!
//! Each type is looked at once, at its shallowest level, so a graph whose types share parts
//! costs its size, not its number of paths, and the answer is the one a walk of every path to
//! that depth gives. The level limit is what stops a generic type that grows as it recurses
//! (`class A<T> { next: A<T[]> }`); a wide graph of any size is searched completely.

use std::collections::VecDeque;

use rustc_hash::FxHashSet;

use crate::ctx::Ctx;
use crate::hir::TyId;

/// What a search does at one type.
pub(crate) enum Look {
    /// This is what the search looks for.
    Found,
    /// Nothing below this type counts.
    Stop,
    /// Search these parts of it.
    Parts(Vec<TyId>),
}

impl Ctx<'_> {
    /// The first type within `levels` levels of `t` (`t` itself is level 0) for which `look`
    /// says [`Look::Found`], searched breadth-first, through the parts `look` gives.
    pub(crate) fn find_within(
        &mut self,
        t: TyId,
        levels: u32,
        look: &mut dyn FnMut(&mut Self, TyId) -> Look,
    ) -> Option<TyId> {
        // Most types have no parts to search: no queue for them.
        let parts = match look(self, t) {
            Look::Found => return Some(t),
            Look::Parts(parts) if levels > 0 && !parts.is_empty() => parts,
            Look::Stop | Look::Parts(_) => return None,
        };
        let mut seen = Seen::default();
        seen.insert(t);
        let mut queue = VecDeque::new();
        for p in parts {
            if seen.insert(p) {
                queue.push_back((p, 1));
            }
        }
        while let Some((t, level)) = queue.pop_front() {
            match look(self, t) {
                Look::Found => return Some(t),
                Look::Stop => {}
                Look::Parts(parts) if level < levels => {
                    for p in parts {
                        if seen.insert(p) {
                            queue.push_back((p, level + 1));
                        }
                    }
                }
                Look::Parts(_) => {}
            }
        }
        None
    }
}

/// The types a search has queued: a list while there are few (most searches), a set after.
#[derive(Default)]
struct Seen {
    few: Vec<TyId>,
    many: FxHashSet<TyId>,
}

impl Seen {
    /// Adds `t`; false if it was there.
    fn insert(&mut self, t: TyId) -> bool {
        const FEW: usize = 16;
        if self.few.len() < FEW {
            if self.few.contains(&t) {
                return false;
            }
            self.few.push(t);
            return true;
        }
        if self.many.is_empty() {
            self.many.extend(self.few.iter().copied());
        }
        self.many.insert(t)
    }
}
