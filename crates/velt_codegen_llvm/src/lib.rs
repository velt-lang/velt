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
mod statics;
mod string_compare;
mod strings;
mod target;
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
    let triple = target::normalize(&opts.target)?;
    let start = Instant::now();
    let ir = checked_ir(program, &triple, opts.optimize)?;
    timings.push(("ir", start.elapsed()));
    let start = Instant::now();
    let (step, obj) = if triple.is_wasm() {
        ("llc", llc::compile(&ir, &triple, opts.optimize))
    } else {
        ("clang", clang::compile(&ir, &triple, opts.optimize))
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
    let triple = target::normalize(&opts.target)?;
    let count = if triple.is_wasm() {
        1
    } else {
        units::unit_count(program, units)
    };
    if count == 1 {
        return emit_object_timed(program, opts, timings).map(|obj| vec![obj]);
    }
    if let Err(errs) = velt_vir::verify(program) {
        return Err(format!("invalid VIR:\n  {}", errs.join("\n  ")));
    }
    types::validate_aggregates(program)?;
    let start = Instant::now();
    let plan = units::plan(program, count);
    timings.push(("units", start.elapsed()));
    let start = Instant::now();
    let compile = |unit: &units::Unit| -> Result<(Vec<u8>, Duration), String> {
        let ir = module::emit_unit(program, &triple, opts.optimize, unit, &plan.shared)?;
        let start = Instant::now();
        let obj = clang::compile(&ir, &triple, opts.optimize)?;
        Ok((obj, start.elapsed()))
    };
    let results = parallel(&plan.units, compile);
    let mut objects = Vec::with_capacity(results.len());
    let mut slowest = Duration::ZERO;
    for r in results {
        let (obj, clang) = r?;
        slowest = slowest.max(clang);
        objects.push(obj);
    }
    timings.push(("ir + clang (parallel)", start.elapsed()));
    timings.push(("slowest clang", slowest));
    Ok(objects)
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

/// The program as textual LLVM IR for `target` (empty / `native` / `host` = the host triple);
/// what `velt build --emit llvm` prints. Needs no clang.
pub fn emit_ir(program: &vir::Program, target: &str) -> Result<String, String> {
    let triple = target::normalize(target)?;
    checked_ir(program, &triple, false)
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
