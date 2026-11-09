//! Caps on LLVM's inlining, so that clang never merges a program into one huge function.
//!
//! velt_opt's inliner has already decided which calls to inline, with limits on a callee's size
//! and on how large a caller may grow (`velt_opt::inline`). LLVM's inliner knows neither: it
//! inlines an internal function with a single caller whatever its size (the "last call to a
//! static function" bonus). A generated `main` that calls a few hundred `run` functions once each
//! took them all in: on a 100,000-line program, its unit became one function of about 1.2 million
//! IR lines, and clang spent most of a 38-minute release build on it, its passes being
//! superlinear in the size of a function. Such calls stay calls, at no cost worth measuring: the
//! callee is large, so one call more is a small part of what it does.
//!
//! Callees first (bottom-up over the call graph's components), every function gets the size it
//! can reach in LLVM (`grown`: its own, plus what the calls it lets LLVM inline bring along):
//! - a call is `noinline` at the call site when the callee would take its caller past velt_opt's
//!   caller limit (`CALLER_LIMIT`), counting the calls before it, unless the callee is small
//!   (LLVM's ordinary inlining, which grows a caller in proportion to its own size);
//! - a function that can grow past that limit by itself is `noinline` (every call of it would be).
//!
//! Only calls LLVM would inline count: of a small callee (LLVM's ordinary inlining), of std's
//! `@inline` functions, and of an internal function called from this one site, whose address no
//! one takes and that no other unit calls (the bonus above). A larger callee called from several
//! sites stays a call in LLVM anyway, so it takes no room under the limit: counted, such calls
//! kept `parseWith` (one call from `main` in bench/json/parse_value, its constant arguments then
//! folded in) out of a `main` that had room for it.
//!
//! So no function grows in LLVM past `CALLER_LIMIT` plus its small callees. A call that fits
//! under the limit stays LLVM's to inline; one that does not stays a call even in a hot loop
//! (`main` → `parseWith` in bench/json/parse_value: out of line, `parseWith` no longer gets its
//! constant arguments folded in, 1.9% more instructions). Calls within a recursive cycle are left
//! to LLVM, which does not grow functions through them. The sizes are velt_opt's cost (statements
//! and blocks) of the optimized VIR this backend translates, so the caps hold whatever velt_opt
//! did before.

use velt_vir::vir::{self, Callee, Inline, Terminator};

use crate::units::{components, Refs};

/// A call is not inlined when it would take its caller past this size (velt_opt's
/// `CALLER_LIMIT`; a test holds the two equal)...
pub(crate) const CALLER_LIMIT: u64 = 10_000;
/// ...unless the callee is at most this big (velt_opt's `SMALL_COST`, held equal the same way).
pub(crate) const SMALL: u64 = 40;

/// Size estimate of `func`: statements plus terminators (velt_opt's `inline::cost`).
pub(crate) fn cost(func: &vir::Function) -> u64 {
    let stmts = (func.blocks.iter().flat_map(|b| &b.stmts))
        .filter(|s| !matches!(s, vir::Stmt::Nop))
        .count();
    (stmts + func.blocks.len()) as u64
}

/// The caps of a program (module docs).
#[derive(Debug, Default)]
pub(crate) struct Caps {
    /// Per function: the size it can reach when LLVM inlines the calls it may inline.
    pub grown: Vec<u64>,
    /// Per function: the blocks whose call stays a call (ascending).
    pub kept: Vec<Vec<usize>>,
}

impl Caps {
    /// The caps of `program` in one module.
    pub(crate) fn of(program: &vir::Program) -> Caps {
        Caps::of_units(program, &[])
    }

    /// The caps of `program` split into units: `shared[f]`, function `f` is referenced from a
    /// unit other than its own (`units::Shared::funcs`; empty: one module).
    pub(crate) fn of_units(program: &vir::Program, shared: &[bool]) -> Caps {
        let n = program.funcs.len();
        let (sites, taken) = call_sites(program);
        // An internal function called from one site only, whose address no one takes and that
        // no other unit refers to: LLVM inlines it whatever its size.
        let once: Vec<bool> = (0..n)
            .map(|g| {
                program.funcs[g].linkage == vir::Linkage::Internal
                    && sites[g] == 1
                    && !taken[g]
                    && !shared.get(g).copied().unwrap_or(false)
            })
            .collect();
        let callee = |t: &Terminator| match t {
            Terminator::Call {
                callee: Callee::Func(f),
                ..
            } if (f.0 as usize) < n => Some(f.0 as usize),
            _ => None,
        };
        let succ: Vec<Vec<usize>> = (program.funcs.iter())
            .map(|f| f.blocks.iter().filter_map(|b| callee(&b.term)).collect())
            .collect();
        let (comp, count) = components(&succ);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&f| comp[f]);
        debug_assert!(order.iter().all(|&f| comp[f] < count));
        let mut caps = Caps {
            grown: vec![0; n],
            kept: vec![Vec::new(); n],
        };
        // Whether LLVM may inline a function at all (its own attributes, function/mod.rs).
        let inlinable = |f: &vir::Function| {
            let returns = (f.blocks.iter()).any(|b| matches!(b.term, Terminator::Return(_)));
            !f.cold && returns && f.inline != Inline::Never && !f.blocks.is_empty()
        };
        for f in order {
            let func = &program.funcs[f];
            let mut total = cost(func);
            for (b, block) in func.blocks.iter().enumerate() {
                let Some(g) = callee(&block.term) else {
                    continue;
                };
                let target = &program.funcs[g];
                // A callee within this function's cycle has no size yet; nor does LLVM grow
                // functions through recursive calls.
                if comp[g] == comp[f] || !inlinable(target) {
                    continue;
                }
                let always = target.inline == Inline::Always;
                if !always && caps.too_large(g) {
                    continue;
                }
                let size = caps.grown[g];
                if always || size <= SMALL {
                    total = total.saturating_add(size);
                } else if !once[g] {
                    // LLVM leaves it a call: no room taken, no cap needed.
                } else if total.saturating_add(size) <= CALLER_LIMIT {
                    total = total.saturating_add(size);
                } else {
                    caps.kept[f].push(b);
                }
            }
            caps.grown[f] = total;
        }
        caps
    }

    /// Whether function `f` (unmarked by std) gets `noinline`.
    pub(crate) fn too_large(&self, f: usize) -> bool {
        self.grown.get(f).is_some_and(|&g| g > CALLER_LIMIT)
    }

    /// The blocks of function `f` whose call stays a call.
    pub(crate) fn kept(&self, f: usize) -> &[usize] {
        self.kept.get(f).map_or(&[], Vec::as_slice)
    }
}

/// Per function: the number of direct call sites of it, and whether its address is taken (by
/// code or by a static: a vtable entry).
fn call_sites(program: &vir::Program) -> (Vec<u32>, Vec<bool>) {
    let n = program.funcs.len();
    let mut sites = vec![0u32; n];
    let mut taken = vec![false; n];
    for f in &program.funcs {
        for b in &f.blocks {
            if let Terminator::Call {
                callee: Callee::Func(g),
                ..
            } = &b.term
            {
                if let Some(s) = sites.get_mut(g.0 as usize) {
                    *s = s.saturating_add(1);
                }
            }
        }
        for g in Refs::of_function(f).addresses {
            if let Some(t) = taken.get_mut(g) {
                *t = true;
            }
        }
    }
    for s in &program.statics {
        for g in Refs::of_static(s).funcs() {
            if let Some(t) = taken.get_mut(g) {
                *t = true;
            }
        }
    }
    (sites, taken)
}
