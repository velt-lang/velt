//! VIR → VIR mid-level optimizer.
//!
//! Cranelift neither inlines nor does much scalar optimization, so this crate provides what
//! rustc's MIR passes plus LLVM's inliner would: the driver runs it after `velt_vir::lower`
//! (+ `verify`) and before `velt_codegen_cl::emit_object`.
//!
//! Passes (one module each; every pass keeps the VIR invariants from `vir.rs`):
//! - `gated_clones`: before the rounds, a gated function's calls of ordinary callees that check
//!   its CPU extension go to clones gated on it, where the check is `true` (#1330).
//! - `frozen`: before the rounds, from `inline` right after a function's callees are inlined
//!   into it (their reads of its params are its own then, while it still has the params; the
//!   loads then move with it into its own callers) and with `noalias` after the rounds, scalar
//!   fields behind `frozen`
//!   params (borrowed values, also ones holding atomics) that the function reads in a loop are
//!   loaded once on entry: nothing changes them during the call, not even atomic operations,
//!   fences or calls.
//! - `bounds`: in functions with `frozen` params, before the rounds (after a cleanup that gives
//!   `a || b` its two branches) and from `inline` once their callees are inlined, range checks
//!   that an earlier check proves (a node's whole range checked once, then words read at
//!   constant offsets in it) are decided, with no-wrap reasoning LLVM cannot do on Velt's
//!   wrapping integers; a frozen param's field (an array's length) is one value however often
//!   it is read, and the decided checks then go wherever the function is inlined.
//! - `inline`: bottom-up inlining of small / called-once direct callees.
//! - `const_fields`: constant fields of aggregate locals (closure code pointers) propagated
//!   into their reads, also through read-only pointers, plus cloning of callees that receive
//!   a known closure (specialization), so closure calls become direct calls.
//! - `vtable_loads`: loads of a new object's vtable pointer and of method slots of static
//!   vtables become constants, so `constfold` turns virtual calls on objects of a known class
//!   into direct calls.
//! - `constfold`: sparse conditional constant propagation + folding, branch folding,
//!   devirtualization of calls through constant function pointers.
//! - `addr_forward`: places through a pointer that always holds `&a…` name `a…` directly.
//! - `heap_sroa`: heap objects that never escape the function (`new` of a small class whose
//!   methods were inlined) live in aggregate locals instead; no allocation, no free.
//! - `enum_sroa`: splits enum locals (a throwing callee's `Result` once inlined) used only
//!   through their tag and variant fields into one local per slot, so the payload is a value
//!   the scalar passes track.
//! - `sroa`: splits aggregate locals whose address is never taken into per-field locals.
//! - `copyprop`: forwards `a = b` copies of register-like scalar locals.
//! - `dce`: removes stores to never-read locals, then the locals themselves.
//! - `thread_tags`: a jump into a block that only tests a local aggregate's field (the tag of
//!   an inlined callee's `Result`) goes to the test's target directly when the jumping block
//!   stored a constant there, so each return of an inlined throwing callee continues on its own
//!   path.
//! - `simplify_cfg`: jump threading, unreachable-block removal, block merging, renumbering.
//! - `dead_funcs`: drops internal functions unreachable from exported ones.
//! - `module_state`: once the rounds are done, a function asks the runtime for the module state
//!   block once per slot range rather than at every access to a module-level `let` (#1571).
//! - `map_probe`: once the rounds are done, a `Map` probe (`lookup`, the key's string hash)
//!   that repeats an earlier one on the same map and an equal key reuses its result, so an
//!   inlined `m.set(k, (m.get(k) ?? 0) + 1)` probes once (#563).
//! - `borrowed_copies`: once the rounds are done, a string local that only reads a copy of a
//!   string (`const src = this.src`) borrows it instead of sharing it, when nothing can change
//!   the source while the copy's text is read: no `velt_rt_str_clone` / `velt_rt_str_drop`
//!   pair (#1670). Before that, over the whole program callees first (`counted`), the same for
//!   counted pointers (`const data = this.db.data`, an array or class object): no count
//!   increment and release when no call that may free a counted object runs while the copy is
//!   used, and the reference never leaves it (#1715).
//! - `noalias`: once the rounds are done, scalar fields behind `noalias` params (modified arrays
//!   and structs) are kept in locals (loaded once, stored back around calls that receive the
//!   param), followed by one scalar cleanup round.
//! - `numrep`: after the rounds (so it also sees the fields `heap_sroa` and `sroa` turned into
//!   locals) and in debug builds, `f64` locals whose values are provably whole numbers within
//!   ±2^53 (intervals, integrality, NaN and `-0` facts, branch refinement, widening) become
//!   `i32`/`i64` locals computed with integer operations, and `i64` locals that only hold 32-bit
//!   values become `i32` (design #525, steps 1 and 2); a `number[]` that never leaves its
//!   function and only holds such values stores its elements as `i64` (#1599, step 5).
//! - `divisions`: after the rounds, signed divisions / remainders by constants whose dividend
//!   is provably non-negative or a multiple of the divisor become shifts, masks or unsigned ops.
//! - `frame_slots`: at the same point, scalar fields of an async frame that a poll function
//!   uses inside a loop are kept in locals (loaded on entry, stored back at every suspension).
//! - `dead_fills`: at the same point, the zero fill of a new object is dropped when every
//!   field is written after it before anything can read the object.
//! - `invariant_reads`: then, a read through object and array header pointers (a typed
//!   array's buffer, its bytes' length) inside a loop nest that writes no memory, calls
//!   nothing and tests only counters (no type test) is made once in front of the outermost such
//!   loop, each pointer tested for null first, so LLVM sees the bound of an inner loop's check
//!   defined outside every loop.
//! - `cold_calls`: last, the calls of a function to one callee that never returns (the
//!   out-of-bounds report, a throw helper) become one call that the call sites jump to, so a
//!   small function with many checks stays cheap enough for LLVM to inline.
//!
//! Every pass keeps `Function::locs` aligned with the statements it edits (`srclocs`).
//!
//! Shared analyses: `visit` (operand/place/successor traversal), `locals` (per-local usage),
//! `callgraph` (call graph + SCCs), `scc` (strongly connected components of any graph),
//! `fresh` (the code right after an allocation, while the new block is private).
//! `interp` (feature `interp`) is a reference interpreter used to check that optimization
//! preserves behaviour.

use velt_vir::vir;

mod addr_forward;
mod borrowed_copies;
mod bounds;
mod callgraph;
mod cold_calls;
mod const_fields;
mod constfold;
mod copyprop;
mod dce;
mod dead_fills;
mod dead_funcs;
mod divisions;
mod enum_sroa;
mod frame_slots;
mod fresh;
mod frozen;
mod gated_clones;
mod heap_sroa;
mod inline;
mod invariant_reads;
mod locals;
mod map_probe;
mod module_state;
mod noalias;
mod numrep;
mod scc;
mod simplify_cfg;
mod srclocs;
mod sroa;
mod thread_tags;
mod timings;
mod visit;
mod vtable_loads;

pub use callgraph::used_externs;
pub use numrep::Unnarrowed;
pub use timings::PassTimings;

/// The `number` variables inside loops of the (optimized) `program` that stay doubles, with
/// why, per function symbol (`velt build --report numbers`).
pub fn number_report(program: &vir::Program) -> Vec<(String, Vec<Unnarrowed>)> {
    let env = numrep::Env::of(
        &program.externs,
        &program.funcs,
        &program.aggs,
        &program.statics,
    );
    program
        .funcs
        .iter()
        .map(|f| (f.symbol.clone(), numrep::unnarrowed(&env, f)))
        .filter(|(_, u)| !u.is_empty())
        .collect()
}

#[cfg(any(test, feature = "interp"))]
pub mod interp;

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod testkit;

#[cfg(test)]
mod import_tests;

/// The functions with code: the per-function passes leave a declaration (an `Import` without
/// a body, vir.rs invariant 11) alone.
fn with_bodies(funcs: &mut [vir::Function]) -> impl Iterator<Item = &mut vir::Function> {
    funcs.iter_mut().filter(|f| !f.blocks.is_empty())
}

/// How hard to optimize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptLevel {
    /// Debug builds: only cheap passes (CFG simplification, the int32 helpers inlined,
    /// `numrep`, unused function removal).
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
            numrep::declare_formatters(&mut program.externs);
            let helpers = numrep::int32_helpers(&program.funcs);
            if helpers.contains(&true) {
                t.time("inline", || inline::run_helpers(program, &helpers));
            }
            let env = numrep::Env::of(
                &program.externs,
                &program.funcs,
                &program.aggs,
                &program.statics,
            );
            for func in with_bodies(&mut program.funcs) {
                t.time("simplify_cfg", || simplify_cfg::run(func));
                if t.time("numrep", || numrep::run(&env, func)) {
                    numrep_cleanup(&program.aggs, func, t);
                }
            }
        }
        OptLevel::Speed => {
            t.time("dead_funcs", || dead_funcs::run(program));
            t.time("gated_clones", || gated_clones::run(program));
            numrep::declare_formatters(&mut program.externs);
            // Once before inlining as well: an array is still one value there, whose length
            // bounds the index loops over it (`numrep` knows a length is below 2^53); after
            // inlining, `sroa` splits it into locals the length fact no longer reaches.
            let env = numrep::Env::of(
                &program.externs,
                &program.funcs,
                &program.aggs,
                &program.statics,
            );
            let signatures = t.time("signatures", || callgraph::signatures(program));
            for func in with_bodies(&mut program.funcs) {
                if t.time("numrep", || numrep::run(&env, func)) {
                    numrep_cleanup(&program.aggs, func, t);
                }
                t.time("frozen", || frozen::run(&program.aggs, func));
                if func.param_attrs.iter().any(|a| a.frozen) {
                    // `a || b` as two branches (threaded), the shape `bounds` reads facts from.
                    t.time("constfold", || constfold::run(&signatures, func));
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(&program.aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                    t.time("bounds", || bounds::run(&program.aggs, func));
                }
            }
            let mut budget = inline::Budget::for_program(program);
            let mut specs = const_fields::Specializations::default();
            for _ in 0..MAX_ROUNDS {
                if !speed_round(program, &mut budget, &mut specs, t) {
                    break;
                }
            }
            let borrowed = t.time("borrowed_copies", || borrowed_copies::counted::run(program));
            for (func, _) in program.funcs.iter_mut().zip(borrowed).filter(|(_, b)| *b) {
                // The releases' tests now branch to one block either way.
                t.time("simplify_cfg", || simplify_cfg::run(func));
                t.time("copyprop", || copyprop::run(func));
                t.time("dce", || dce::run(&program.aggs, func));
                t.time("simplify_cfg", || simplify_cfg::run(func));
            }
            let allocator = heap_sroa::Allocator::find(program);
            let probes = map_probe::Probes::find(program);
            let lookups = module_state::Lookups::find(program);
            let runtime = borrowed_copies::Runtime::find(program);
            let env = numrep::Env::of(
                &program.externs,
                &program.funcs,
                &program.aggs,
                &program.statics,
            );
            for func in with_bodies(&mut program.funcs) {
                if t.time("module_state", || module_state::run(lookups, func)) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("map_probe", || map_probe::run(&program.aggs, &probes, func)) {
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(&program.aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("borrowed_copies", || {
                    borrowed_copies::run(&program.aggs, &runtime, func)
                }) {
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(&program.aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("numrep", || numrep::run(&env, func)) {
                    numrep_cleanup(&program.aggs, func, t);
                }
                t.time("divisions", || divisions::run(func));
                t.time("dead_fills", || {
                    dead_fills::run(&program.aggs, allocator, func)
                });
            }
            promote_memory(program, t);
            let kinds = invariant_reads::Kinds::of(&program.aggs, &program.externs);
            for func in with_bodies(&mut program.funcs) {
                if t.time("invariant_reads", || {
                    invariant_reads::run(&program.aggs, &kinds, func)
                }) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
            }
            let no_return = cold_calls::NoReturn::find(program);
            for func in &mut program.funcs {
                if t.time("cold_calls", || cold_calls::run(&no_return, func)) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
            }
        }
    }
    t.time("dead_funcs", || dead_funcs::run(program));
}

/// After `numrep` changed `func`: forward the copies it made, fold what that exposed (an
/// integer compared with itself), and drop what became dead.
fn numrep_cleanup(aggs: &[vir::AggLayout], func: &mut vir::Function, t: &mut PassTimings) {
    t.time("copyprop", || copyprop::run(func));
    if t.time("numrep", || numrep::self_comparisons(func)) {
        t.time("simplify_cfg", || simplify_cfg::run(func));
    }
    t.time("dce", || dce::run(aggs, func));
}

/// Promote `noalias` and `frozen` pointees and async frame slots (after inlining settled which
/// bodies they live in), then clean up the loads and stores it moved.
fn promote_memory(program: &mut vir::Program, t: &mut PassTimings) {
    let signatures = t.time("signatures", || callgraph::signatures(program));
    for func in with_bodies(&mut program.funcs) {
        let mut changed = t.time("noalias", || noalias::run(&program.aggs, func));
        changed |= t.time("frozen", || frozen::run(&program.aggs, func));
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
    for func in with_bodies(&mut program.funcs) {
        changed |= t.time("vtable_loads", || {
            vtable_loads::run(&program.aggs, &program.statics, allocator, func)
        });
        changed |= t.time("constfold", || constfold::run(&signatures, func));
        changed |= t.time("copyprop", || copyprop::run(func));
        changed |= t.time("addr_forward", || addr_forward::run(&program.aggs, func));
        changed |= t.time("heap_sroa", || {
            heap_sroa::run(&program.aggs, allocator, func)
        });
        changed |= t.time("enum_sroa", || enum_sroa::run(&program.aggs, func));
        changed |= t.time("sroa", || sroa::run(&program.aggs, func));
        changed |= t.time("dce", || dce::run(&program.aggs, func));
        changed |= t.time("thread_tags", || thread_tags::run(&program.aggs, func));
        changed |= t.time("simplify_cfg", || simplify_cfg::run(func));
    }
    changed
}
