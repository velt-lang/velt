//! VIR → native object file via Cranelift.
//! The public API below is a contract (maintainer-owned).
//!
//! Modules:
//! - `isa`: target triple → configured Cranelift ISA (PIC, stack probes, frame pointers); `cpu`:
//!   the CPU it is for (`--cpu`) and the features code for it needs.
//! - `module`: `build_module<M: Module>` declares/defines a whole program in any Cranelift
//!   module (the object backend and the JIT share the exact same path); `parallel`: its
//!   functions translated and compiled on every core, defined in order.
//! - `dev`: [`DevSession`], the in-process JIT behind `velt dev`, with hot swap; `c_symbols`:
//!   the C library functions it resolves by address; `jit_memory`: one contiguous arena per JIT
//!   module.
//! - `abi`: VIR type → Cranelift type / C-ABI signature mapping, layout lookups.
//! - `function`: per-function translation (places, operands, ops, casts, terminators).
//! - `entry`: the `main` of executables linked against the shared runtime ([`emit_entry_object`]).
//! - `debug_info`: DWARF line tables (objects, and JIT code through the GDB JIT interface).
//! - `unwind`: unwind tables (Windows x64/arm64 `.pdata`/`.xdata`, ELF/Mach-O eh_frame), and their
//!   run-time registration for JIT code.

use std::time::{Duration, Instant};

use cranelift_object::{ObjectBuilder, ObjectModule};
use velt_vir::vir;

/// Early-return an `Err(String)` built with `format!`.
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

mod abi;
mod c_libraries;
#[cfg(unix)]
mod c_symbols;
mod cache;
mod cpu;
mod cpu_names;
mod debug_info;
mod dev;
mod entry;
mod function;
mod isa;
mod jit_memory;
mod module;
mod parallel;
mod tls;
mod unwind;
#[cfg(windows)]
mod win_symbols;

#[cfg(test)]
mod tests;

pub use cache::{CacheStats, CodeCache};
pub use cpu::{
    arm_features_from_macros, check_cpu, ext_x86_features, is_for_another_arch, level_features,
    required_features, x86_features_from_macros, ARM_FEATURES, DEFAULT_X86_CPU, X86_FEATURES,
    X86_FEATURE_MACROS,
};
pub use dev::{DevSession, JitProgram, Reload};

/// Internal result type: errors are human-readable messages.
pub(crate) type CodegenResult<T> = Result<T, String>;

/// Options for `emit_object`.
#[derive(Clone, Debug)]
pub struct CodegenOptions {
    /// LLVM-style target triple, e.g. `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`,
    /// `x86_64-unknown-linux-gnu`.
    pub target: String,
    /// Enable Cranelift optimizations (`opt_level = "speed"`).
    pub optimize: bool,
    /// `--cpu` (`baseline`, `x86-64-v2`, `native`, an LLVM CPU name; see `cpu`); `None`: the
    /// default, [`DEFAULT_X86_CPU`] on x86_64 (`native` when `target` is empty or `native`).
    pub cpu: Option<String>,
}

/// CONTRACT: emit a relocatable object file (COFF / Mach-O / ELF per target) for the program.
pub fn emit_object(program: &vir::Program, opts: &CodegenOptions) -> Result<Vec<u8>, String> {
    emit_object_cached(program, opts, None).map(|(obj, _)| obj)
}

/// [`emit_object`], compiling the functions on every core and taking those compiled before (by
/// any build on this machine) from `cache`; also says how many came from it.
pub fn emit_object_cached(
    program: &vir::Program,
    opts: &CodegenOptions,
    cache: Option<&dyn CodeCache>,
) -> Result<(Vec<u8>, CacheStats), String> {
    if let Err(errs) = velt_vir::verify(program) {
        return Err(format!("invalid VIR:\n  {}", errs.join("\n  ")));
    }
    emit_object_timed(program, opts, cache, &mut Vec::new())
}

/// [`emit_object_cached`] for a program that passed `velt_vir::verify` already (the driver
/// verifies after optimizing), so it is not verified again, appending the time of each step to
/// `timings` (`velt build --timings`). VIR that would not pass may fail with an ICE here.
pub fn emit_object_timed(
    program: &vir::Program,
    opts: &CodegenOptions,
    cache: Option<&dyn CodeCache>,
    timings: &mut Vec<(&'static str, Duration)>,
) -> Result<(Vec<u8>, CacheStats), String> {
    emit_object_on(program, opts, cache, timings, velt_vir::parallel::jobs())
}

/// [`emit_object_timed`] for a caller that is done with the program: each function's code (its
/// blocks and their source locations) is freed once every function is compiled, before the
/// object and its debug information are written, the moment a debug build needs the most
/// memory. Its declarations stay (debug information describes their locals and types).
pub fn emit_object_freeing(
    program: &mut vir::Program,
    opts: &CodegenOptions,
    cache: Option<&dyn CodeCache>,
    timings: &mut Vec<(&'static str, Duration)>,
) -> Result<(Vec<u8>, CacheStats), String> {
    let jobs = velt_vir::parallel::jobs();
    emit_object_from(Program::Freeing(program), opts, cache, timings, jobs)
}

/// [`emit_object_timed`] on `jobs` threads (verified VIR).
fn emit_object_on(
    program: &vir::Program,
    opts: &CodegenOptions,
    cache: Option<&dyn CodeCache>,
    timings: &mut Vec<(&'static str, Duration)>,
    jobs: usize,
) -> Result<(Vec<u8>, CacheStats), String> {
    emit_object_from(Program::Borrowed(program), opts, cache, timings, jobs)
}

/// The program [`emit_object_from`] compiles, and whether it may free the functions' code.
enum Program<'a> {
    Borrowed(&'a vir::Program),
    Freeing(&'a mut vir::Program),
}

impl Program<'_> {
    fn get(&self) -> &vir::Program {
        match self {
            Program::Borrowed(p) => p,
            Program::Freeing(p) => p,
        }
    }
}

/// Compile a verified program on `jobs` threads.
fn emit_object_from(
    mut program: Program<'_>,
    opts: &CodegenOptions,
    cache: Option<&dyn CodeCache>,
    timings: &mut Vec<(&'static str, Duration)>,
    jobs: usize,
) -> Result<(Vec<u8>, CacheStats), String> {
    let isa = isa::make_isa(&opts.target, opts.cpu.as_deref(), opts.optimize, false)?;
    let builder = ObjectBuilder::new(
        isa.clone(),
        "velt",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| format!("codegen: cannot create object builder: {e}"))?;
    let mut module = ObjectModule::new(builder);
    let built = module::build_module_cached(
        &mut module,
        &*isa,
        program.get(),
        &module::Naming::Program,
        cache,
        jobs,
    )?;
    timings.extend_from_slice(&built.timings);
    if let Program::Freeing(p) = &mut program {
        for f in &mut p.funcs {
            f.blocks = Vec::new();
            f.locs = Vec::new();
        }
    }
    let program = program.get();
    let start = Instant::now();
    let mut product = module.finish();
    unwind::add_unwind_info(&mut product, &*isa, &built.unwind)?;
    debug_info::object::add_debug_info(&mut product, program, &built.lines)?;
    let obj = product
        .emit()
        .map_err(|e| format!("codegen: cannot write object file: {e}"))?;
    timings.push(("object", start.elapsed()));
    Ok((obj, built.cache))
}

/// The entry object of an executable linked against the shared runtime library (debug builds):
/// a C `main(argc, argv)` returning `velt_rt_start(argc, argv, velt_main)`. Additive (compile-speed
/// stream); the static runtime defines `main` itself.
pub fn emit_entry_object(target: &str) -> Result<Vec<u8>, String> {
    entry::emit_entry_object(target)
}

/// CONTRACT: the triple of the machine the compiler runs on. On Linux the C library is the one
/// `velt` itself was built for (`-musl` for a static Alpine build, `-gnu` otherwise): programs
/// link against the runtime library shipped next to it, built for the same environment.
pub fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        _ if cfg!(target_env = "musl") => format!("{arch}-unknown-linux-musl"),
        _ => format!("{arch}-unknown-linux-gnu"),
    }
}
