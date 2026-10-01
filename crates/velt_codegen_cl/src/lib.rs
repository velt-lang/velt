//! VIR → native object file via Cranelift.
//! The public API below is a contract (maintainer-owned).
//!
//! Modules:
//! - `isa`: target triple → configured Cranelift ISA (PIC, stack probes, frame pointers).
//! - `module`: `build_module<M: Module>` declares/defines a whole program in any Cranelift
//!   module (the object backend and the JIT share the exact same path).
//! - `dev`: [`DevSession`], the in-process JIT behind `velt dev`, with hot swap; `c_symbols`:
//!   the C library functions it resolves by address.
//! - `abi`: VIR type → Cranelift type / C-ABI signature mapping, layout lookups.
//! - `function`: per-function translation (places, operands, ops, casts, terminators).
//! - `entry`: the `main` of executables linked against the shared runtime ([`emit_entry_object`]).
//! - `unwind`: unwind tables (Windows x64/arm64 `.pdata`/`.xdata`, ELF/Mach-O eh_frame), and their
//!   run-time registration for JIT code.

use cranelift_object::{ObjectBuilder, ObjectModule};
use velt_vir::vir;

/// Early-return an `Err(String)` built with `format!`.
macro_rules! bail {
    ($($t:tt)*) => { return Err(format!($($t)*)) };
}

mod abi;
#[cfg(unix)]
mod c_symbols;
mod dev;
mod entry;
mod function;
mod isa;
mod module;
mod unwind;

#[cfg(test)]
mod tests;

pub use dev::{DevSession, HandlerCode, JitProgram, Reload};

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
}

/// CONTRACT: emit a relocatable object file (COFF / Mach-O / ELF per target) for the program.
pub fn emit_object(program: &vir::Program, opts: &CodegenOptions) -> Result<Vec<u8>, String> {
    if let Err(errs) = velt_vir::verify(program) {
        return Err(format!("invalid VIR:\n  {}", errs.join("\n  ")));
    }
    let isa = isa::make_isa(&opts.target, opts.optimize, false)?;
    let builder = ObjectBuilder::new(
        isa.clone(),
        "velt",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| format!("codegen: cannot create object builder: {e}"))?;
    let mut module = ObjectModule::new(builder);
    let built = module::build_module(&mut module, program, &module::Naming::Program)?;
    let mut product = module.finish();
    unwind::add_unwind_info(&mut product, &*isa, &built.unwind)?;
    product
        .emit()
        .map_err(|e| format!("codegen: cannot write object file: {e}"))
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
