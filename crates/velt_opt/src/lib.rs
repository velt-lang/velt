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
//! - `fits_pair`: once the rounds are done (inlining left `fits` as bare externs),
//!   `fits(a, j) && fits(b, j)` with the same lane type and index becomes one test against the
//!   smaller length, one exit test for a loop over two arrays.
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
//! - `cold_calls`: then, the calls of a function to one callee that never returns (the
//!   out-of-bounds report, a throw helper) become one call that the call sites jump to, so a
//!   small function with many checks stays cheap enough for LLVM to inline.
//! - `null_exits`: after it, a call of a function that returns at once when a pointer it is
//!   passed points at null (every drop glue) first tests that pointer, so a null field or a
//!   value moved out costs no call, and none where the value is known.
//!
//! Every pass keeps `Function::locs` aligned with the statements it edits (`srclocs`).
//!
//! Shared analyses: `visit` (operand/place/successor traversal), `locals` (per-local usage),
//! `callgraph` (call graph + SCCs), `scc` (strongly connected components of any graph),
//! `fresh` (the code right after an allocation, while the new block is private).
//! `interp` (feature `interp`) is a reference interpreter used to check that optimization
//! preserves behaviour.

use std::collections::HashSet;

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
mod fits_pair;
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
mod null_exits;
mod numrep;
mod parallel;
mod scc;
mod simplify_cfg;
mod srclocs;
mod sroa;
mod thread_tags;
mod timings;
mod visit;
mod vtable_loads;

pub use callgraph::used_externs;
/// The inliner's caller limit and small-callee size, which the LLVM backend's caps on LLVM's
/// inlining mirror (`velt_codegen_llvm::inlining`, whose tests hold them equal).
pub use inline::{CALLER_LIMIT as INLINE_CALLER_LIMIT, SMALL_COST as INLINE_SMALL_COST};
pub use numrep::{Unnarrowed, Why as UnnarrowedWhy};
pub use timings::PassTimings;

/// Per function of `funcs`: is it one a debug build inlines into its callers ([`OptLevel::None`]):
/// the compiler's small helpers and std's trivial functions, whose calls never reach the backend.
pub fn debug_inlined(funcs: &[vir::Function]) -> Vec<bool> {
    let mut helpers = numrep::int32_helpers(funcs);
    inline::std_forwarders(funcs, &mut helpers);
    helpers
}

/// What `velt dev` keeps of the debug pipeline's run over a program ([`optimize_debug_kept`]),
/// to optimize only the functions a later version changes ([`optimize_debug_delta`]) to what
/// the whole run would make of them.
pub struct DebugKept {
    /// The helpers inlined into their callers ([`debug_inlined`]).
    helpers: Vec<bool>,
    /// Their bodies as lowered, which the inliner copies.
    helper_bodies: rustc_hash::FxHashMap<usize, vir::Function>,
    /// Each function's part of the parameter facts, after inlining (`numrep::params`).
    calls: Vec<numrep::params::Calls>,
    /// The parameter facts the run used.
    params: numrep::params::ParamFacts,
}

/// The debug pipeline ([`OptLevel::None`]) over `program`, keeping what
/// [`optimize_debug_delta`] needs.
pub fn optimize_debug_kept(program: &mut vir::Program, t: &mut PassTimings) -> DebugKept {
    let helpers = debug_inlined(&program.funcs);
    let helper_bodies = (helpers.iter().enumerate())
        .filter(|(_, h)| **h)
        .map(|(i, _)| (i, program.funcs[i].clone()))
        .collect();
    if helpers.contains(&true) {
        t.time("inline", || inline::run_helpers(program, &helpers));
    }
    let calls: Vec<_> = program.funcs.iter().map(numrep::params::calls_of).collect();
    let params = numrep::params::merge(&program.funcs, &program.statics, &calls);
    debug_passes(program, params.clone(), (0..program.funcs.len()).collect(), t);
    DebugKept {
        helpers,
        helper_bodies,
        calls,
        params,
    }
}

/// The debug pipeline over the functions `changed` of `program`, whose other functions are as
/// the last run over it ([`optimize_debug_kept`]) left them. Refused, and nothing is changed,
/// when that would change what the run makes of another function: a changed function is (or
/// was) a helper the others inline, or the calls change a parameter fact.
pub fn optimize_debug_delta(
    program: &mut vir::Program,
    kept: &mut DebugKept,
    changed: &[usize],
    t: &mut PassTimings,
) -> Result<(), String> {
    let n = program.funcs.len();
    // The helpers of the changed functions, as the whole run would find them.
    let now = debug_inlined_at(&program.funcs, &kept.helpers, changed);
    for &i in changed {
        let was = kept.helpers.get(i).copied().unwrap_or(false);
        if was || now.contains(&i) {
            return Err(format!("`{}` is inlined into its callers", program.funcs[i].symbol));
        }
    }
    kept.helpers.resize(n, false);
    let mut calls = kept.calls.clone();
    calls.resize(n, Default::default());
    let mut inlined: Vec<(usize, vir::Function)> = vec![];
    for &i in changed {
        let before = program.funcs[i].clone();
        inline::run_helpers_into(program, i, &kept.helpers, &kept.helper_bodies);
        calls[i] = numrep::params::calls_of(&program.funcs[i]);
        inlined.push((i, before));
    }
    let params = numrep::params::merge(&program.funcs, &program.statics, &calls);
    if params != kept.params {
        for (i, before) in inlined {
            program.funcs[i] = before;
        }
        return Err("the calls change what a parameter is known to hold".into());
    }
    kept.calls = calls;
    debug_passes(program, params, changed.to_vec(), t);
    Ok(())
}

/// Which of the functions `changed` [`debug_inlined`] would make helpers, the others' as
/// `helpers` says.
fn debug_inlined_at(funcs: &[vir::Function], helpers: &[bool], changed: &[usize]) -> Vec<usize> {
    let int32 = numrep::int32_helpers(funcs);
    let mut mask: Vec<bool> = (0..funcs.len())
        .map(|i| helpers.get(i).copied().unwrap_or(false) || int32[i])
        .collect();
    for &i in changed {
        mask[i] = int32[i];
    }
    let before = mask.clone();
    inline::std_forwarders(funcs, &mut mask);
    (changed.iter().copied())
        .filter(|&i| (mask[i] && !before[i]) || int32[i])
        .collect()
}

/// The debug pipeline's per-function passes over the functions `which`, with the parameter
/// facts `params`.
fn debug_passes(
    program: &mut vir::Program,
    params: numrep::params::ParamFacts,
    which: Vec<usize>,
    t: &mut PassTimings,
) {
    let env = numrep::Env::with_params(&program.externs, &program.funcs, &program.aggs, params);
    let mut wanted = vec![false; program.funcs.len()];
    which.iter().for_each(|&i| wanted[i] = true);
    // Each function on its own, on every core: the passes read only `env` and the aggregates.
    // A pass's time is then the sum over the threads.
    let mut bodies: Vec<&mut vir::Function> = (program.funcs.iter_mut().enumerate())
        .filter(|(i, f)| wanted[*i] && !f.blocks.is_empty())
        .map(|(_, f)| f)
        .collect();
    let aggs = &program.aggs;
    let per_thread = parallel::for_each(&mut bodies, PassTimings::new, |t, func| {
        t.time("simplify_cfg", || simplify_cfg::run(func));
        if t.time("numrep", || numrep::run(&env, func)) {
            numrep_cleanup(aggs, func, t);
        }
    });
    per_thread.iter().for_each(|thread| t.add(thread));
}

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

/// The functions whose bodies the per-function passes leave alone ([`optimize_served`]): by
/// symbol, as functions are renumbered and cloned while the program is optimized.
#[derive(Clone, Copy, Default)]
pub(crate) struct Served<'a>(Option<&'a HashSet<String>>);

impl<'s> Served<'s> {
    /// Whether the per-function passes optimize `f`'s body.
    pub(crate) fn optimizes(self, f: &vir::Function) -> bool {
        self.0.is_none_or(|served| !served.contains(&f.symbol))
    }

    /// The functions of `funcs` with code ([`with_bodies`]) that the per-function passes
    /// optimize.
    fn bodies<'f>(
        self,
        funcs: &'f mut [vir::Function],
    ) -> impl Iterator<Item = &'f mut vir::Function> + use<'f, 's> {
        with_bodies(funcs).filter(move |f| self.optimizes(f))
    }
}

/// Remove the internal functions nothing exported reaches any more (`dead_funcs`), renumbering
/// the others: after functions were made declarations once optimized (veltc's std cache), what
/// only they called. Returns whether any were removed.
pub fn remove_unused_functions(program: &mut vir::Program) -> bool {
    dead_funcs::run(program)
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
    optimize_with(program, level, t, Served::default());
}

/// [`optimize_timed`] of a program whose functions `served` names become declarations once it is
/// optimized, their code linked from elsewhere (veltc's std cache: a release build's `@noInline`
/// std functions). Their bodies stay in the program for what the rest of it sees of them: the
/// inliner still inlines their callees into them, which spends its program-wide budget as without
/// them served, their calls count, and they keep alive what they call. The first per-function
/// pass, before inlining, works on them too (their bodies are small then, and the inliner's budget
/// starts from the sizes it leaves); after that none does (no `constfold`, `sroa`, `dce`,
/// `bounds`, …), as that work would be on code that is dropped. A clone made of one (`null_exits`,
/// `const_fields`) is the program's own, made from the body as it is then.
pub fn optimize_served(
    program: &mut vir::Program,
    level: OptLevel,
    t: &mut PassTimings,
    served: &HashSet<String>,
) {
    optimize_with(program, level, t, Served(Some(served)));
}

/// [`optimize_timed`], leaving the bodies of `served` to the inliner alone.
fn optimize_with(program: &mut vir::Program, level: OptLevel, t: &mut PassTimings, served: Served) {
    match level {
        OptLevel::None => {
            let helpers = debug_inlined(&program.funcs);
            if helpers.contains(&true) {
                t.time("inline", || inline::run_helpers(program, &helpers));
            }
            let params = numrep::params::of(&program.funcs, &program.statics);
            debug_passes(program, params, (0..program.funcs.len()).collect(), t);
        }
        OptLevel::Speed => {
            t.time("dead_funcs", || dead_funcs::run(program));
            t.time("gated_clones", || gated_clones::run(program));
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
            let fold = constfold::Program::of(&signatures);
            let aggs = &program.aggs;
            // Served bodies too: they are small before inlining, and the budget starts from the
            // sizes this pass leaves (`optimize_served`).
            each_function(with_bodies(&mut program.funcs), t, |t, func| {
                if t.time("numrep", || numrep::run(&env, func)) {
                    numrep_cleanup(aggs, func, t);
                }
                t.time("frozen", || frozen::run(aggs, func));
                if func.param_attrs.iter().any(|a| a.frozen) {
                    // `a || b` as two branches (threaded), the shape `bounds` reads facts from.
                    t.time("constfold", || constfold::run(&fold, func));
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                    t.time("bounds", || bounds::run(aggs, func));
                }
                false
            });
            let mut budget = inline::Budget::for_program(program);
            let mut specs = const_fields::Specializations::default();
            for _ in 0..MAX_ROUNDS {
                if !speed_round(program, &mut budget, &mut specs, served, t) {
                    break;
                }
            }
            // What inlining left without callers, before the passes below go over every function.
            t.time("dead_funcs", || dead_funcs::run(program));
            let paired = t.time("fits_pair", || fits_pair::run(program));
            for f in paired {
                let func = &mut program.funcs[f];
                if !served.optimizes(func) {
                    continue;
                }
                t.time("simplify_cfg", || simplify_cfg::run(func));
                t.time("dce", || dce::run(&program.aggs, func));
            }
            let borrowed = t.time("borrowed_copies", || borrowed_copies::counted::run(program));
            let changed = (program.funcs.iter_mut().zip(borrowed)).filter(|(_, b)| *b);
            for (func, _) in changed.filter(|(f, _)| served.optimizes(f)) {
                // The releases' tests now branch to one block either way.
                t.time("simplify_cfg", || simplify_cfg::run(func));
                t.time("copyprop", || copyprop::run(func));
                t.time("dce", || dce::run(&program.aggs, func));
                t.time("simplify_cfg", || simplify_cfg::run(func));
            }
            let allocator = heap_sroa::Allocator::find(program);
            let probes = map_probe::Probes::find(program);
            let lookups = module_state::Lookups::find(program);
            // The read-only summaries of every function (`borrowed_copies/read_only.rs`).
            let runtime = t.time("read_only", || borrowed_copies::Runtime::find(program));
            let env = numrep::Env::of(
                &program.externs,
                &program.funcs,
                &program.aggs,
                &program.statics,
            );
            let aggs = &program.aggs;
            each_function(served.bodies(&mut program.funcs), t, |t, func| {
                if t.time("module_state", || module_state::run(lookups, func)) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("map_probe", || map_probe::run(aggs, &probes, func)) {
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("borrowed_copies", || {
                    borrowed_copies::run(aggs, &runtime, func)
                }) {
                    t.time("copyprop", || copyprop::run(func));
                    t.time("dce", || dce::run(aggs, func));
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                if t.time("numrep", || numrep::run(&env, func)) {
                    numrep_cleanup(aggs, func, t);
                }
                t.time("divisions", || divisions::run(func));
                t.time("dead_fills", || dead_fills::run(aggs, allocator, func));
                false
            });
            promote_memory(program, served, t);
            let kinds = invariant_reads::Kinds::of(&program.aggs, &program.externs);
            let aggs = &program.aggs;
            each_function(served.bodies(&mut program.funcs), t, |t, func| {
                if t.time("invariant_reads", || {
                    invariant_reads::run(aggs, &kinds, func)
                }) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                false
            });
            let no_return = cold_calls::NoReturn::find(program);
            let funcs = program.funcs.iter_mut().filter(|f| served.optimizes(f));
            each_function(funcs, t, |t, func| {
                if t.time("cold_calls", || cold_calls::run(&no_return, func)) {
                    t.time("simplify_cfg", || simplify_cfg::run(func));
                }
                false
            });
            t.time("null_exits", || null_exits::run(program));
        }
    }
    t.time("dead_funcs", || dead_funcs::run(program));
}

/// `work` on each of `funcs`, on several threads when there are enough functions (`parallel`):
/// a per-function pass reads the rest of the program only, so each function sees the same inputs
/// whatever the number of threads. A pass's time is the sum over the threads. Whether `work`
/// changed any function.
fn each_function<'f>(
    funcs: impl Iterator<Item = &'f mut vir::Function>,
    t: &mut PassTimings,
    work: impl Fn(&mut PassTimings, &mut vir::Function) -> bool + Sync,
) -> bool {
    let mut funcs: Vec<&mut vir::Function> = funcs.collect();
    let per_thread = parallel::for_each(
        &mut funcs,
        || (PassTimings::new(), false),
        |(t, changed), func| *changed |= work(t, func),
    );
    let mut changed = false;
    for (thread, c) in &per_thread {
        t.add(thread);
        changed |= c;
    }
    changed
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
fn promote_memory(program: &mut vir::Program, served: Served, t: &mut PassTimings) {
    let signatures = t.time("signatures", || callgraph::signatures(program));
    let fold = constfold::Program::of(&signatures);
    let aggs = &program.aggs;
    // A program with cleanup pads (it catches) keeps promoted memory current on unwind edges.
    let resume = (program.externs.iter())
        .position(|e| velt_vir::eh::is_resume(&e.symbol))
        .map(|i| vir::ExternId(i as u32));
    each_function(served.bodies(&mut program.funcs), t, |t, func| {
        let mut changed = t.time("noalias", || noalias::run(aggs, func, resume));
        changed |= t.time("frozen", || frozen::run(aggs, func));
        changed |= t.time("frame_slots", || frame_slots::run(aggs, func, resume));
        if changed {
            t.time("constfold", || constfold::run(&fold, func));
            t.time("copyprop", || copyprop::run(func));
            t.time("dce", || dce::run(aggs, func));
            t.time("simplify_cfg", || simplify_cfg::run(func));
        }
        false
    });
}

/// One round of the `Speed` pipeline; returns whether anything changed.
fn speed_round(
    program: &mut vir::Program,
    budget: &mut inline::Budget,
    specs: &mut const_fields::Specializations,
    served: Served,
    t: &mut PassTimings,
) -> bool {
    let mut changed = t.time("inline", || inline::run(program, budget, served));
    changed |= t.time("const_fields", || const_fields::run(program, specs));
    let signatures = t.time("signatures", || callgraph::signatures(program));
    let fold = constfold::Program::of(&signatures);
    let allocator = heap_sroa::Allocator::find(program);
    // A function inlining left without callers (one called once, now in its caller) is removed
    // at the end: optimizing it again would only double the work on its body.
    let live = t.time("dead_funcs", || dead_funcs::live_functions(program));
    let funcs = (program.funcs.iter_mut().zip(live))
        .filter(|(f, l)| *l && !f.blocks.is_empty() && served.optimizes(f));
    let (aggs, statics) = (&program.aggs, &program.statics);
    changed |= each_function(funcs.map(|(f, _)| f), t, |t, func| {
        let mut changed = t.time("vtable_loads", || {
            vtable_loads::run(aggs, statics, allocator, func)
        });
        changed |= t.time("constfold", || constfold::run(&fold, func));
        changed |= t.time("copyprop", || copyprop::run(func));
        changed |= t.time("addr_forward", || addr_forward::run(aggs, func));
        changed |= t.time("heap_sroa", || heap_sroa::run(aggs, allocator, func));
        changed |= t.time("enum_sroa", || enum_sroa::run(aggs, func));
        changed |= t.time("sroa", || sroa::run(aggs, func));
        changed |= t.time("dce", || dce::run(aggs, func));
        changed |= t.time("thread_tags", || thread_tags::run(aggs, func));
        changed |= t.time("simplify_cfg", || simplify_cfg::run(func));
        changed
    });
    changed
}
