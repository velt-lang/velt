//! VIR → native object file via LLVM (release backend).
//!
//! The backend prints textual LLVM IR (`.ll`) and hands it to `clang -O3 -c -x ir`, so no LLVM
//! libraries are linked into the compiler: all that is needed at build time is a `clang`
//! executable (see [`available`]). The public API mirrors `velt_codegen_cl` exactly, and so do
//! the semantics (wrapping integers, masked shifts, saturating float→int casts, NaN-aware
//! comparisons, `MIN / -1` wrapping, C ABI sign/zero extension of narrow integers).
//!
//! Modules:
//! - `target`: triple validation and per-target function attributes.
//! - `types`: VIR type → LLVM type / ABI attribute mapping, constant and symbol spelling.
//! - `module`: whole-program IR (extern declarations, functions, attribute groups).
//! - `runtime`: known runtime functions (math intrinsics, memory effects, allocator results).
//! - `strings`: inline fast paths for the hot runtime string functions (drop, `==`, `<`, one-byte
//!   slices), called instead of the runtime function.
//! - `statics`: read-only data, including relocated address slots (vtables).
//! - `function`: per-function translation (places, operands, ops, casts, terminators).
//! - `debug`: `!dbg` metadata (CodeView on Windows, DWARF elsewhere) when the VIR carries
//!   source locations (`vir::Program::files`; debug builds and `velt build -g`).
//! - `units`: splitting large programs into codegen units compiled in parallel.
//! - `clang`: locating `clang` and running it on the emitted IR.
//! - `llc`: locating LLVM's `opt`/`llc` and running them for the WebAssembly targets.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use velt_vir::vir;

/// Early-return an `Err(String)` built with `format!`.
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

mod clang;
mod debug;
mod function;
mod llc;
mod module;
mod rounding;
mod runtime;
mod simd;
mod statics;
mod string_compare;
mod strings;
mod target;
mod tbaa;
mod types;
mod units;

#[cfg(test)]
mod tests;

pub use clang::{available, find_clang, rejected_clang};
pub use llc::{find_wasm_tools, LlvmTools};
pub use target::is_wasm;
/// Options for [`emit_object`]: the same struct as the Cranelift backend's, so the driver can
/// pass one value to either backend.
pub use velt_codegen_cl::CodegenOptions;
use velt_codegen_cl::{CacheStats, CodeCache};

/// Internal result type: errors are human-readable messages.
pub(crate) type CodegenResult<T> = Result<T, String>;

/// Emit a relocatable object file (COFF / Mach-O / ELF / WebAssembly per `opts.target`) for the
/// program by compiling its LLVM IR with clang, or `opt` + `llc` for WebAssembly (`-O3` when
/// `opts.optimize`, else `-O0`).
pub fn emit_object(program: &vir::Program, opts: &CodegenOptions) -> Result<Vec<u8>, String> {
    emit_object_timed(program, opts, &mut vec![])
}

/// [`emit_object`], appending the time of its steps (`ir`: printing the module, `clang` or
/// `llc`: compiling it) to `timings` (for `velt build --timings`).
pub fn emit_object_timed(
    program: &vir::Program,
    opts: &CodegenOptions,
    timings: &mut Vec<(&'static str, Duration)>,
) -> Result<Vec<u8>, String> {
    emit_object_cached(program, opts, timings, None).map(|(obj, _)| obj)
}

/// [`emit_object_timed`], taking the object from `cache` when clang compiled the same IR before.
fn emit_object_cached(
    program: &vir::Program,
    opts: &CodegenOptions,
    timings: &mut Vec<(&'static str, Duration)>,
    cache: Option<&dyn CodeCache>,
) -> Result<(Vec<u8>, bool), String> {
    let triple = target::normalize(&opts.target)?.with_cpu(opts.cpu.as_deref())?;
    let start = Instant::now();
    let ir = checked_ir(program, &triple, opts.optimize)?;
    timings.push(("ir", start.elapsed()));
    let start = Instant::now();
    let (step, obj) = if triple.is_wasm() {
        (
            "llc",
            llc::compile(&ir, &triple, opts.optimize).map(|obj| (obj, false)),
        )
    } else {
        (
            "clang",
            clang::compile_cached(&ir, &triple, opts.optimize, cache),
        )
    };
    timings.push((step, start.elapsed()));
    obj
}

/// [`emit_object_timed`] for large programs: one object per codegen unit, compiled by parallel
/// clang processes (see `units`), at most one per core. `units`: how many (`None`: from the size
/// of the program, one below about 32 000 VIR statements). WebAssembly targets always get one
/// object.
pub fn emit_objects_timed(
    program: &vir::Program,
    opts: &CodegenOptions,
    units: Option<usize>,
    timings: &mut Vec<(&'static str, Duration)>,
) -> Result<Vec<Vec<u8>>, String> {
    emit_objects_cached(program, opts, units, timings, None).map(|(objs, _)| objs)
}

/// [`emit_objects_timed`], taking each unit's object from `cache` when clang compiled the same IR
/// with the same flags before (in any build on this machine), and storing it there otherwise; also
/// says how many units came from it.
pub fn emit_objects_cached(
    program: &vir::Program,
    opts: &CodegenOptions,
    units: Option<usize>,
    timings: &mut Vec<(&'static str, Duration)>,
    cache: Option<&dyn CodeCache>,
) -> Result<(Vec<Vec<u8>>, CacheStats), String> {
    let triple = target::normalize(&opts.target)?.with_cpu(opts.cpu.as_deref())?;
    let count = if triple.is_wasm() {
        1
    } else {
        units::unit_count(program, units)
    };
    if count == 1 {
        let (obj, hit) = emit_object_cached(program, opts, timings, cache)?;
        return Ok((vec![obj], CacheStats::of(&[hit])));
    }
    if let Err(errs) = velt_vir::verify(program) {
        return Err(format!("invalid VIR:\n  {}", errs.join("\n  ")));
    }
    types::validate_aggregates(program)?;
    let start = Instant::now();
    let plan = units::plan(program, count);
    timings.push(("units", start.elapsed()));
    let start = Instant::now();
    let compile = |unit: &units::Unit| -> Result<(Vec<u8>, bool, Duration), String> {
        let ir = module::emit_unit(program, &triple, opts.optimize, unit, &plan.shared)?;
        let start = Instant::now();
        let (obj, hit) = clang::compile_cached(&ir, &triple, opts.optimize, cache)?;
        Ok((obj, hit, start.elapsed()))
    };
    let results = parallel(&plan.units, compile);
    let mut objects = Vec::with_capacity(results.len());
    let mut hits = Vec::with_capacity(results.len());
    let mut slowest = Duration::ZERO;
    for r in results {
        let (obj, hit, clang) = r?;
        slowest = slowest.max(clang);
        objects.push(obj);
        hits.push(hit);
    }
    timings.push(("ir + clang (parallel)", start.elapsed()));
    timings.push(("slowest clang", slowest));
    Ok((objects, CacheStats::of(&hits)))
}

/// `f` of every item, in order, on at most one thread per core (each takes the next item left).
fn parallel<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(&T) -> Result<R, String> + Sync,
) -> Vec<Result<R, String>> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, Result<R, String>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..cores.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else {
                            return out;
                        };
                        out.push((i, f(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| {
                w.join().unwrap_or_else(|_| {
                    vec![(usize::MAX, Err("ICE: codegen unit thread panicked".into()))]
                })
            })
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// The program as textual LLVM IR for `target` (empty / `native` / `host` = the host triple) and
/// its default CPU. Needs no clang.
pub fn emit_ir(program: &vir::Program, target: &str) -> Result<String, String> {
    let triple = target::normalize(target)?;
    checked_ir(program, &triple, false)
}

/// The program as textual LLVM IR for `opts.target` and `opts.cpu`, as clang compiles it for an
/// object (unoptimized): what `velt build --emit llvm` prints.
pub fn emit_ir_for(program: &vir::Program, opts: &CodegenOptions) -> Result<String, String> {
    let triple = target::normalize(&opts.target)?.with_cpu(opts.cpu.as_deref())?;
    checked_ir(program, &triple, false)
}

/// The x86 features code from this backend for `cpu` on `target` uses, as a mask over
/// `velt_codegen_cl::X86_FEATURES` (`None`: the default CPU), which the start-up check checks:
/// the level's for a level, the host's for `native`, and for a CPU name what clang enables for
/// it (`-march=`), which is not always what Cranelift's preset of that name does.
pub fn required_features(target: &str, cpu: Option<&str>) -> Result<u32, String> {
    match (velt_codegen_cl::level_features(target, cpu)?, cpu) {
        (Some(mask), _) => Ok(mask),
        (None, Some(cpu)) => clang::x86_features(&target::normalize(target)?.triple, cpu),
        (None, None) => Err("ICE: the default CPU is a level".into()),
    }
}

/// Verify `program` and print its module (`optimized` only marks the debug compile unit).
fn checked_ir(
    program: &vir::Program,
    triple: &target::Target,
    optimized: bool,
) -> Result<String, String> {
    if let Err(errs) = velt_vir::verify(program) {
        return Err(format!("invalid VIR:\n  {}", errs.join("\n  ")));
    }
    module::emit_module(program, triple, optimized)
}

/// The triple of the machine the compiler runs on (same as the Cranelift backend's).
pub fn host_triple() -> String {
    velt_codegen_cl::host_triple()
}
