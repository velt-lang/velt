//! VIR → VIR mid-level optimizer.
//!
//! Cranelift neither inlines nor does much scalar optimization, so this crate provides what
//! rustc's MIR passes plus LLVM's inliner would: the driver runs it after `velt_vir::lower`
//! (+ `verify`) and before `velt_codegen_cl::emit_object`.
//!
//! Passes (one module each; every pass keeps the VIR invariants from `vir.rs`):
//! - `inline`: bottom-up inlining of small / called-once direct callees.
//! - `const_fields`: constant fields of aggregate locals (closure code pointers) propagated
//!   into their reads, also through read-only pointers, plus cloning of callees that receive
//!   a known closure (specialization), so closure calls become direct calls.
//! - `constfold`: sparse conditional constant propagation + folding, branch folding,
//!   devirtualization of calls through constant function pointers.
//! - `addr_forward`: places through a pointer that always holds `&a…` name `a…` directly.
//! - `heap_sroa`: heap objects that never escape the function (`new` of a small class whose
//!   methods were inlined) live in aggregate locals instead; no allocation, no free.
//! - `sroa`: splits aggregate locals whose address is never taken into per-field locals.
//! - `copyprop`: forwards `a = b` copies of register-like scalar locals.
//! - `dce`: removes stores to never-read locals, then the locals themselves.
//! - `simplify_cfg`: jump threading, unreachable-block removal, block merging, renumbering.
//! - `dead_funcs`: drops internal functions unreachable from exported ones.
//! - `noalias`: once the rounds are done, scalar fields behind `noalias` params (modified arrays
//!   and structs) are kept in locals (loaded once, stored back around calls that receive the
//!   param), followed by one scalar cleanup round.
//! - `numrep`: after the rounds, `f64`/`i64` locals that only ever hold 32-bit integers (the
//!   results of `x | 0` and friends) become `i32` locals, and ToInt32 of a sum of two of them
//!   becomes a 32-bit add (design #525, step 1).
//! - `divisions`: after the rounds, signed divisions / remainders by constants whose dividend
//!   is provably non-negative or a multiple of the divisor become shifts, masks or unsigned ops.
//! - `frame_slots`: at the same point, scalar fields of an async frame that a poll function
//!   uses inside a loop are kept in locals (loaded on entry, stored back at every suspension).
//!
//! Every pass keeps `Function::locs` aligned with the statements it edits (`srclocs`).
//!
//! Shared analyses: `visit` (operand/place/successor traversal), `locals` (per-local usage),
//! `callgraph` (call graph + SCCs), `scc` (strongly connected components of any graph).
//! `interp` (feature `interp`) is a reference interpreter used to check that optimization
//! preserves behaviour.

use velt_vir::vir;

mod addr_forward;
mod callgraph;
mod const_fields;
mod constfold;
mod copyprop;
mod dce;
mod dead_funcs;
mod divisions;
mod frame_slots;
mod heap_sroa;
mod inline;
mod locals;
mod noalias;
mod numrep;
mod scc;
mod simplify_cfg;
mod srclocs;
mod sroa;
mod timings;
mod visit;

pub use timings::PassTimings;

#[cfg(any(test, feature = "interp"))]
pub mod interp;

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod testkit;

/// How hard to optimize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    /// Debug builds: only cheap cleanups (CFG simplification, unused function removal).
    None,
    /// Release builds: the full pass pipeline.
    Speed,
}

/// Maximum number of rounds of the `Speed` pipeline; each round is cheap, and later rounds
/// mostly clean up what inlining exposed.
const MAX_ROUNDS: usize = 3;

/// Runs the pass pipeline in place. Output must satisfy velt_vir::verify.
pub fn optimize(program: &mut vir::Program, level: OptLevel) {
    optimize_timed(program, level, &mut PassTimings::new());
}

/// [`optimize`], adding each pass's time to `t` (for `velt build --timings`).
pub fn optimize_timed(program: &mut vir::Program, level: OptLevel, t: &mut PassTimings) {
    match level {
        OptLevel::None => {
            for func in &mut program.funcs {
                t.time("simplify_cfg", || simplify_cfg::run(func));
            }
        }
        OptLevel::Speed => {
            t.time("dead_funcs", || dead_funcs::run(program));
            let mut budget = inline::Budget::for_program(program);
            let mut specs = const_fields::Specializations::default();
            for _ in 0..MAX_ROUNDS {
                if !speed_round(program, &mut budget, &mut specs, t) {
                    break;
                }
            }
            for func in &mut program.funcs {
                if t.time("numrep", || numrep::run(&program.externs, func)) {
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(&program.aggs, func));
                }
                t.time("divisions", || divisions::run(func));
            }
            promote_memory(program, t);
        }
    }
    t.time("dead_funcs", || dead_funcs::run(program));
}

/// Promote `noalias` pointees and async frame slots (after inlining settled which bodies they
/// live in), then clean up the loads and stores it moved.
fn promote_memory(program: &mut vir::Program, t: &mut PassTimings) {
    let signatures = t.time("signatures", || callgraph::signatures(program));
    for func in &mut program.funcs {
        let mut changed = t.time("noalias", || noalias::run(&program.aggs, func));
        changed |= t.time("frame_slots", || frame_slots::run(&program.aggs, func));
        if changed {
            t.time("constfold", || constfold::run(&signatures, func));
            t.time("copyprop", || copyprop::run(func));
            t.time("dce", || dce::run(&program.aggs, func));
            t.time("simplify_cfg", || simplify_cfg::run(func));
        }
    }
}

/// One round of the `Speed` pipeline; returns whether anything changed.
fn speed_round(
    program: &mut vir::Program,
    budget: &mut inline::Budget,
    specs: &mut const_fields::Specializations,
    t: &mut PassTimings,
) -> bool {
    let mut changed = t.time("inline", || inline::run(program, budget));
    changed |= t.time("const_fields", || const_fields::run(program, specs));
    let signatures = t.time("signatures", || callgraph::signatures(program));
    let allocator = heap_sroa::Allocator::find(program);
    for func in &mut program.funcs {
        changed |= t.time("constfold", || constfold::run(&signatures, func));
        changed |= t.time("copyprop", || copyprop::run(func));
        changed |= t.time("addr_forward", || addr_forward::run(&program.aggs, func));
        changed |= t.time("heap_sroa", || {
            heap_sroa::run(&program.aggs, allocator, func)
        });
        changed |= t.time("sroa", || sroa::run(&program.aggs, func));
        changed |= t.time("dce", || dce::run(&program.aggs, func));
        changed |= t.time("simplify_cfg", || simplify_cfg::run(func));
    }
    changed
}
