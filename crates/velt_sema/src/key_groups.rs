//! The record key types a type contains, also through the fields of structs, classes and unions
//! (`record_keys::check_instantiations` asks for every type a function mentions).
//!
//! Every type is walked once: results are kept across calls, and the types of a recursive group
//! (a strongly connected part of the type graph) contain the same keys, so the group's result is
//! taken when its walk ends, for all its types (Tarjan's algorithm). A result does not depend on
//! where the walk entered the group or what was asked before, and a type graph whose types share
//! parts costs its size, not its number of paths.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, Pass};
use crate::hir::{DefId, TyId, TyKind};
use crate::types::children;

/// The most types one walk visits: a generic type that grows as it recurses
/// (`class A<T> { next: A<T[]> }`) has no end. A result the limit cut short is not kept.
const MAX_WALK: usize = 4096;

/// The record key types `t` contains; `memo` holds the complete results of earlier calls.
pub(crate) fn keys_in(cx: &mut Ctx, t: TyId, memo: &mut FxHashMap<TyId, Vec<TyId>>) -> Vec<TyId> {
    if let Some(ks) = memo.get(&t) {
        return ks.clone();
    }
    let mut step = |t: TyId| {
        let kind = cx.ty.kind(t).clone();
        let mut parts = children(&kind);
        if let Some((k, _)) = cx.record_type_args(t) {
            return (Some(k), parts);
        }
        if let TyKind::Adt(d, args) = &kind {
            for m in member_types(cx, *d) {
                parts.push(cx.subst(m, args));
            }
        }
        (None, parts)
    };
    let (keys, visited) = walk(t, memo, MAX_WALK, &mut step);
    // The types walked: the scaling tests compare this between program sizes.
    add_work(Pass::RecordKeys, visited as u64);
    keys
}

/// Field types of a struct, class or object type; payload types of an enum (generic).
fn member_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
        DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// What a type is to a walk: the key it is (a `Record`'s key type), and the types it contains.
type Step<'s> = dyn FnMut(TyId) -> (Option<TyId>, Vec<TyId>) + 's;

/// The keys `t` reaches by `step`, and how many types the walk visited (at most `limit`). The
/// complete results of every type walked go into `memo`.
fn walk(
    t: TyId,
    memo: &mut FxHashMap<TyId, Vec<TyId>>,
    limit: usize,
    step: &mut Step<'_>,
) -> (Vec<TyId>, usize) {
    let mut w = Walk {
        memo,
        open: FxHashMap::default(),
        stack: vec![],
        cut_short: FxHashMap::default(),
        visited: 0,
        limit,
    };
    let keys = w.visit(t, step).keys;
    (keys, w.visited)
}

/// One walk.
struct Walk<'a> {
    /// Complete results.
    memo: &'a mut FxHashMap<TyId, Vec<TyId>>,
    /// The types whose group is still being walked, with their visiting number.
    open: FxHashMap<TyId, usize>,
    /// Those types, in visiting order.
    stack: Vec<TyId>,
    /// Results of this walk that the limit cut short: used in it, never kept.
    cut_short: FxHashMap<TyId, Vec<TyId>>,
    visited: usize,
    limit: usize,
}

/// What a type's walk found: its keys (those of its whole group once `low` is its own visiting
/// number), the smallest visiting number of an open type it reaches, and whether the limit cut
/// it short.
struct Found {
    keys: Vec<TyId>,
    low: usize,
    cut: bool,
}

fn found(keys: Vec<TyId>, low: usize, cut: bool) -> Found {
    Found { keys, low, cut }
}

impl Walk<'_> {
    fn visit(&mut self, t: TyId, step: &mut Step<'_>) -> Found {
        if let Some(ks) = self.memo.get(&t) {
            return found(ks.clone(), usize::MAX, false);
        }
        if let Some(ks) = self.cut_short.get(&t) {
            return found(ks.clone(), usize::MAX, true);
        }
        if let Some(&n) = self.open.get(&t) {
            // In the group being walked: its keys join the group's when the group ends.
            return found(vec![], n, false);
        }
        if self.visited >= self.limit {
            return found(vec![], usize::MAX, true);
        }
        let n = self.visited;
        self.visited += 1;
        self.open.insert(t, n);
        self.stack.push(t);
        let (key, parts) = step(t);
        let mut f = found(key.into_iter().collect(), n, false);
        for p in parts {
            let r = self.visit(p, step);
            f.low = f.low.min(r.low);
            f.cut |= r.cut;
            for k in r.keys {
                if !f.keys.contains(&k) {
                    f.keys.push(k);
                }
            }
        }
        if f.low == n {
            // `t` starts a group: it ends here, with the keys of all its types.
            while let Some(m) = self.stack.pop() {
                self.open.remove(&m);
                let done = match f.cut {
                    true => &mut self.cut_short,
                    false => &mut *self.memo,
                };
                done.insert(m, f.keys.clone());
                if m == t {
                    break;
                }
            }
            f.low = usize::MAX;
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A type graph: per type, the key it is and the types it contains.
    struct Graph(Vec<(Option<u32>, Vec<u32>)>);

    impl Graph {
        fn keys(&self, t: u32, memo: &mut FxHashMap<TyId, Vec<TyId>>, limit: usize) -> Vec<u32> {
            let mut step = |t: TyId| {
                let (k, parts) = &self.0[t.0 as usize];
                (k.map(TyId), parts.iter().map(|&p| TyId(p)).collect())
            };
            let mut ks: Vec<u32> = walk(TyId(t), memo, limit, &mut step)
                .0
                .iter()
                .map(|k| k.0)
                .collect();
            ks.sort_unstable();
            ks
        }
    }

    /// 0 → 1 → 2 → 0 (a group) with keys at 1 and 2, then 2 → 3 → key 4; 5 → 1.
    fn cycle() -> Graph {
        Graph(vec![
            (None, vec![1]),
            (Some(10), vec![2]),
            (Some(11), vec![0, 3]),
            (None, vec![4]),
            (Some(12), vec![]),
            (None, vec![1]),
        ])
    }

    /// Every type of a recursive group has the group's keys, whichever type is asked first:
    /// a type reached while its group is still open is not kept with what the walk had found
    /// by then.
    #[test]
    fn results_do_not_depend_on_where_a_group_is_entered() {
        let g = cycle();
        let want = vec![10, 11, 12];
        for first in 0..6 {
            let mut memo = FxHashMap::default();
            g.keys(first, &mut memo, MAX_WALK);
            for t in [0, 1, 2, 5] {
                assert_eq!(
                    g.keys(t, &mut memo, MAX_WALK),
                    want,
                    "type {t} after {first}"
                );
            }
            assert_eq!(g.keys(3, &mut memo, MAX_WALK), vec![12]);
        }
    }

    /// Shared parts are walked once per call, and once at all with the memo.
    #[test]
    fn shared_parts_are_walked_once() {
        // Type k contains type k-1 twice (2^30 paths from type 30); type 0 is key 100.
        let mut types = vec![(Some(100), vec![])];
        types.extend((1..=30).map(|k| (None, vec![k - 1, k - 1])));
        let g = Graph(types);
        let mut memo = FxHashMap::default();
        let mut step = |t: TyId| {
            let (k, parts) = &g.0[t.0 as usize];
            (k.map(TyId), parts.iter().map(|&p| TyId(p)).collect())
        };
        let (keys, visited) = walk(TyId(30), &mut memo, MAX_WALK, &mut step);
        assert_eq!((keys, visited), (vec![TyId(100)], 31));
        assert_eq!(walk(TyId(30), &mut memo, MAX_WALK, &mut step).1, 0);
    }

    /// A walk the limit cuts short keeps nothing: a later walk with room finds every key.
    #[test]
    fn a_result_cut_short_is_not_kept() {
        let g = cycle();
        let mut memo = FxHashMap::default();
        let short = g.keys(0, &mut memo, 2);
        assert!(short.len() < 3, "{short:?}");
        assert!(memo.keys().all(|t| t.0 == 4 || t.0 == 3), "{memo:?}");
        assert_eq!(g.keys(0, &mut memo, MAX_WALK), vec![10, 11, 12]);
    }
}
