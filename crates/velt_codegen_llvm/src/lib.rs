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
//! - `statics`: read-only data, including relocated address slots (vtables).
//! - `function`: per-function translation (places, operands, ops, casts, terminators).
//! - `debug`: `!dbg` metadata (CodeView on Windows, DWARF elsewhere) when the VIR carries
//!   source locations (`vir::Program::files`; debug builds and `velt build -g`).
//! - `clang`: locating `clang` and running it on the emitted IR.
//! - `llc`: locating LLVM's `opt`/`llc` and running them for the WebAssembly targets.

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
mod runtime;
mod statics;
mod target;
mod types;

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
