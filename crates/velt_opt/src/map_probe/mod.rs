//! Repeated `Map` probes reuse the first one's result (#563).
//!
//! Every `Map` method (std/prelude/map.vlt) probes with `lookup(key, hash(key) * FIB)`, so
//! `m.set(k, (m.get(k) ?? 0) + 1)`, `m.has(k) ? m.get(k)! : …` or a `Record` update hash the key
//! and walk the probe sequence twice once the methods are inlined. Both are pure: `lookup` only
//! reads the map, and `velt_rt_str_hash` only reads the string. This pass replaces a call `P` to
//! one of them by a copy of the result of an earlier call `Q` to the same function when:
//! - `Q` dominates `P` and its result is still in its destination at `P` (a local written only
//!   by `Q`);
//! - the arguments are equal ([`equiv`]): the same values, or for strings passed by pointer the
//!   same text (a key `set` clones from the one `get` hashed counts);
//! - nothing on a path from `Q` to `P` can write the memory they read ([`region`]): no store
//!   through a pointer and no call other than the read-only probes themselves and
//!   `velt_rt_str_clone` into a local (which writes the new value and the source's reference
//!   count, never the text).
//!
//! It runs once inlining has settled, so the two probes of a fused `get` + `set` sit in one
//! function; copy propagation and dead-code elimination then remove the duplicated hash
//! arithmetic.

mod equiv;
mod facts;
mod memory;
pub(crate) mod region;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use velt_vir::vir::{
    AggLayout, Callee, ExternId, FuncId, Function, Operand, Place, Program, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::srclocs::push_stmt;
use facts::Cx;
use region::Point;

/// Prefix of the mangled symbol of every `Map<K, V>.lookup` instance (and its clones).
const LOOKUP_PREFIX: &str = "_V3stdP7preludeP3mapN3MapM6lookup_";

/// How many of the nearest earlier probes a probe is compared with. The search also stops at
/// the first one with a write in between, which every farther one has too.
const CANDIDATES: usize = 4;

/// Runtime functions that only read memory: the probes, and the string comparisons a `lookup`
/// inlined into the caller would make.
const READ_ONLY_EXTERNS: [&str; 3] = ["velt_rt_str_hash", "velt_rt_str_eq", "velt_rt_str_cmp"];

/// `velt_rt_str_clone(src, out)`: writes `*out` and `src`'s reference count.
const STR_CLONE: &str = "velt_rt_str_clone";

/// The functions this pass knows, by id in one program.
pub(crate) struct Probes {
    /// `Map.lookup` instances (pure, may be reused) and their parameter types.
    lookups: HashMap<FuncId, Vec<Ty>>,
    /// `velt_rt_str_hash`: pure, may be reused.
    hash: Option<ExternId>,
    /// Externs that write no memory.
    read_only: HashSet<ExternId>,
    clone: Option<ExternId>,
    /// `velt_rt_alloc`: returns fresh memory, writes nothing a probe can read.
    alloc: Option<ExternId>,
}

impl Probes {
    /// Find the probe functions of `program`.
    pub(crate) fn find(program: &Program) -> Probes {
        let lookups = (0..program.funcs.len() as u32)
            .map(FuncId)
            .filter(|f| {
                program.funcs[f.0 as usize]
                    .symbol
                    .starts_with(LOOKUP_PREFIX)
            })
            .map(|f| (f, program.funcs[f.0 as usize].params.clone()))
            .collect();
        let ext = |name: &str| {
            program
                .externs
                .iter()
                .position(|e| e.symbol == name)
                .map(|i| ExternId(i as u32))
        };
        Probes {
            lookups,
            hash: ext("velt_rt_str_hash"),
            read_only: READ_ONLY_EXTERNS.iter().filter_map(|n| ext(n)).collect(),
            clone: ext(STR_CLONE),
            alloc: ext("velt_rt_alloc"),
        }
    }

    /// Whether a call to `callee` may be replaced by an earlier one's result.
    fn reusable(&self, callee: &Callee) -> bool {
        match callee {
            Callee::Func(f) => self.lookups.contains_key(f),
            Callee::Extern(e) => Some(*e) == self.hash,
            Callee::Ptr { .. } => false,
        }
    }

    /// Whether a call to `callee` writes no memory.
    fn reads_only(&self, callee: &Callee) -> bool {
        match callee {
            Callee::Func(f) => self.lookups.contains_key(f),
            Callee::Extern(e) => self.read_only.contains(e),
            Callee::Ptr { .. } => false,
        }
    }

    /// Parameter types of a reusable probe.
    fn params(&self, callee: &Callee) -> &[Ty] {
        match callee {
            Callee::Func(f) => self.lookups.get(f).map_or(&[], Vec::as_slice),
            _ => &[Ty::Ptr],
        }
    }

    fn is_alloc(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Extern(e) if Some(*e) == self.alloc)
    }

    fn is_clone(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Extern(e) if Some(*e) == self.clone)
    }
}

/// Reuse repeated probes in `func`; returns whether any call was replaced.
pub(crate) fn run(aggs: &[AggLayout], probes: &Probes, func: &mut Function) -> bool {
    let calls: Vec<usize> = (0..func.blocks.len())
        .filter(|&b| matches!(&func.blocks[b].term, Terminator::Call { callee, .. } if probes.reusable(callee)))
        .collect();
    if calls.len() < 2 {
        return false;
    }
    let reuses = {
        let cx = Cx::new(aggs, func, probes);
        calls
            .iter()
            .filter_map(|&p| {
                let q = cx
                    .earlier_probes(p, &calls)
                    .into_iter()
                    .take(CANDIDATES)
                    .take_while(|&q| cx.quiet_between(q, p))
                    .find(|&q| cx.reusable_for(q, p))?;
                Some((p, cx.result_of(q)?))
            })
            .collect::<Vec<_>>()
    };
    for &(p, result) in &reuses {
        replace_call(func, p, result);
    }
    !reuses.is_empty()
}

/// Turn the probe call ending block `b` into `dest = result; goto next`.
fn replace_call(func: &mut Function, b: usize, result: velt_vir::vir::Local) {
    let Terminator::Call { dest, next, .. } = &func.blocks[b].term else {
        unreachable!("ICE: map_probe replaces only calls");
    };
    let (dest, next) = (dest.clone(), *next);
    let at = func.locs.get(b).and_then(|l| l.last().copied().flatten());
    if let Some(dest) = dest {
        let copy = Stmt::Assign(dest, Rvalue::Use(Operand::Copy(Place::local(result))));
        push_stmt(func, b, copy, at);
    }
    func.blocks[b].term = Terminator::Goto(next);
}

/// The call terminator ending block `b`: callee, arguments and destination.
fn call_at(func: &Function, b: usize) -> Option<(&Callee, &[Operand], &Option<Place>)> {
    match &func.blocks[b].term {
        Terminator::Call {
            callee, args, dest, ..
        } => Some((callee, args, dest)),
        _ => None,
    }
}

/// The point of the terminator of block `b`.
fn term_point(func: &Function, b: usize) -> Point {
    Point {
        block: b,
        index: func.blocks[b].stmts.len(),
    }
}
