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

/// How many times the instances of one generic definition may grow on one path of a walk: an
/// instance counts when its type arguments are larger than those of the nearest instance of the
/// same definition around it on the path. A generic type that grows as it recurses
/// (`class A<T> { next: A<T[]> }`) has no end; a chain of distinct types linked through a
/// wrapper (`Box<C1<K>>` in `C0`, `Box<C2<K>>` in `C1`, …) doesn't grow, however long. A result
/// this limit cut short is not kept.
const MAX_GROWTH: u32 = 64;

/// The most types one walk visits, as a last resort for generic types that grow in many ways at
/// once: far more than a program's types.
const MAX_WALK: usize = 1 << 20;

/// The record key types `t` contains; `memo` holds the complete results of earlier calls.
pub(crate) fn keys_in(cx: &mut Ctx, t: TyId, memo: &mut FxHashMap<TyId, Vec<TyId>>) -> Vec<TyId> {
    if let Some(ks) = memo.get(&t) {
        return ks.clone();
    }
    let mut step = |t: TyId| {
        let kind = cx.ty.kind(t).clone();
        let mut parts = children(&kind);
        if let Some((k, _)) = cx.record_type_args(t) {
            return Look {
                key: Some(k),
                parts,
                generic: None,
            };
        }
        let mut generic = None;
        if let TyKind::Adt(d, args) = &kind {
            generic = (!args.is_empty()).then(|| (d.0, args_size(cx, args)));
            for m in member_types(cx, *d) {
                parts.push(cx.subst(m, args));
            }
        }
        Look {
            key: None,
            parts,
            generic,
        }
    };
    let (keys, visited) = walk(t, memo, LIMITS, &mut step);
    // The types walked: the scaling tests compare this between program sizes.
    add_work(Pass::RecordKeys, visited as u64);
    keys
}

/// How large type arguments `args` are: the number of types written in them, counted up to
/// [`MAX_SIZE`] (a bound on the work; growth past it is still growth by a later instance).
fn args_size(cx: &Ctx, args: &[TyId]) -> u32 {
    let mut n = 0;
    let mut todo = args.to_vec();
    while let Some(t) = todo.pop() {
        n += 1;
        if n >= MAX_SIZE {
            break;
        }
        todo.extend(children(cx.ty.kind(t)));
    }
    n
}

/// The largest type argument size [`args_size`] counts to.
const MAX_SIZE: u32 = 1 << 16;

/// Field types of a struct, class or object type; payload types of an enum (generic).
fn member_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
        DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// What a type is to a walk.
struct Look {
    /// The key it is (a `Record`'s key type).
    key: Option<TyId>,
    /// The types it contains.
    parts: Vec<TyId>,
    /// The generic definition it instantiates, and how large its type arguments are.
    generic: Option<(u32, u32)>,
}

type Step<'s> = dyn FnMut(TyId) -> Look + 's;

/// Where a walk stops: [`MAX_GROWTH`] and [`MAX_WALK`] (smaller in the tests).
#[derive(Clone, Copy)]
struct Limits {
    growth: u32,
    total: usize,
}

const LIMITS: Limits = Limits {
    growth: MAX_GROWTH,
    total: MAX_WALK,
};

/// The keys `t` reaches by `step`, and how many types the walk visited. The complete results of
/// every type walked go into `memo`.
fn walk(
    t: TyId,
    memo: &mut FxHashMap<TyId, Vec<TyId>>,
    limits: Limits,
    step: &mut Step<'_>,
) -> (Vec<TyId>, usize) {
    let mut w = Walk {
        memo,
        open: FxHashMap::default(),
        stack: vec![],
        cut_short: FxHashMap::default(),
        nested: FxHashMap::default(),
        visited: 0,
        limits,
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
    /// Results of this walk that a limit cut short: used in it, never kept.
    cut_short: FxHashMap<TyId, Vec<TyId>>,
    /// Per generic definition, the instances of it on the path being walked, innermost last:
    /// the size of their type arguments and how often they grew up to there.
    nested: FxHashMap<u32, Vec<(u32, u32)>>,
    visited: usize,
    limits: Limits,
}

/// What a type's walk found: its keys (those of its whole group once `low` is its own visiting
/// number), the smallest visiting number of an open type it reaches, and whether a limit cut it
/// short.
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
        if self.visited >= self.limits.total {
            return found(vec![], usize::MAX, true);
        }
        let look = step(t);
        if let Some((g, size)) = look.generic {
            let on_path = self.nested.entry(g).or_default();
            let grown = on_path.last().map_or(0, |&(s, n)| n + u32::from(size > s));
            if grown >= self.limits.growth {
                return found(vec![], usize::MAX, true);
            }
            on_path.push((size, grown));
        }
        let n = self.visited;
        self.visited += 1;
        self.open.insert(t, n);
        self.stack.push(t);
        let mut f = found(look.key.into_iter().collect(), n, false);
        for p in look.parts {
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
        if let Some((g, _)) = look.generic {
            self.nested.entry(g).or_default().pop();
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
        fn step(&self) -> impl FnMut(TyId) -> Look + '_ {
            |t: TyId| {
                let (k, parts) = &self.0[t.0 as usize];
                Look {
                    key: k.map(TyId),
                    parts: parts.iter().map(|&p| TyId(p)).collect(),
                    generic: None,
                }
            }
        }

        fn keys(&self, t: u32, memo: &mut FxHashMap<TyId, Vec<TyId>>, limits: Limits) -> Vec<u32> {
            let mut ks: Vec<u32> = walk(TyId(t), memo, limits, &mut self.step())
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
            g.keys(first, &mut memo, LIMITS);
            for t in [0, 1, 2, 5] {
                assert_eq!(g.keys(t, &mut memo, LIMITS), want, "type {t} after {first}");
            }
            assert_eq!(g.keys(3, &mut memo, LIMITS), vec![12]);
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
        let (keys, visited) = walk(TyId(30), &mut memo, LIMITS, &mut g.step());
        assert_eq!((keys, visited), (vec![TyId(100)], 31));
        assert_eq!(walk(TyId(30), &mut memo, LIMITS, &mut g.step()).1, 0);
    }

    /// A type with 10,000 parts, the last a key, is walked completely: no limit counts the
    /// parts of a finite graph, also when they are all instances of one generic definition
    /// side by side.
    #[test]
    fn wide_graphs_are_walked_completely() {
        let n = 10_000u32;
        let mut step = |t: TyId| match t.0 {
            0 => Look {
                key: None,
                parts: (1..=n).map(TyId).collect(),
                generic: None,
            },
            k => Look {
                key: (k == n).then_some(TyId(u32::MAX)),
                parts: vec![],
                generic: Some((7, 1)),
            },
        };
        let mut memo = FxHashMap::default();
        let (keys, visited) = walk(TyId(0), &mut memo, LIMITS, &mut step);
        assert_eq!((keys, visited), (vec![TyId(u32::MAX)], n as usize + 1));
        assert_eq!(memo.len(), n as usize + 1);
    }

    /// A long chain of instances of one generic definition whose arguments don't grow (type k
    /// holds type k+1, both of argument size 3) is walked completely.
    #[test]
    fn chains_through_one_wrapper_are_walked_completely() {
        let n = 1000;
        let mut step = |t: TyId| Look {
            key: (t.0 == n).then_some(TyId(99)),
            parts: if t.0 < n { vec![TyId(t.0 + 1)] } else { vec![] },
            generic: Some((1, 3)),
        };
        let mut memo = FxHashMap::default();
        let (keys, visited) = walk(TyId(0), &mut memo, LIMITS, &mut step);
        assert_eq!((keys, visited), (vec![TyId(99)], n as usize + 1));
    }

    /// A generic type that grows as it recurses (type k holds type k+1, an instance of the same
    /// definition with larger arguments, without end) is cut short, and its result is not
    /// kept: a later walk with room is not given it.
    #[test]
    fn growing_generics_are_cut_short_and_not_kept() {
        let mut step = |t: TyId| Look {
            key: (t.0 == 3).then_some(TyId(99)),
            parts: vec![TyId(t.0 + 1)],
            generic: Some((1, t.0 + 1)),
        };
        let mut memo = FxHashMap::default();
        let (keys, visited) = walk(TyId(0), &mut memo, LIMITS, &mut step);
        assert_eq!((keys, visited), (vec![TyId(99)], MAX_GROWTH as usize));
        assert!(memo.is_empty(), "{memo:?}");
        // A walk cut short by the total keeps nothing either.
        let g = cycle();
        let tight = Limits {
            growth: MAX_GROWTH,
            total: 2,
        };
        assert!(g.keys(0, &mut memo, tight).len() < 3);
        assert!(memo.keys().all(|t| t.0 == 4 || t.0 == 3), "{memo:?}");
        assert_eq!(g.keys(0, &mut memo, LIMITS), vec![10, 11, 12]);
    }
}
