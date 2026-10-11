//! One program version in its own `JITModule`: compiles the functions the version defines,
//! resolves everything else to code that is already loaded (trampolines, pinned code of earlier
//! versions, runtime symbols, C library functions), and registers the new code's unwind info
//! with the system (Windows x64, macOS, Linux) and its line tables with debuggers (macOS, Linux:
//! `debug_info::jit`).

use std::time::{Duration, Instant};

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir;

use crate::module::{build_module, DevFunction, Naming};

/// Address space reserved for one version's code, data and unwind records (committed up front
/// on Windows, see `jit_memory`): the first version gets the most; later ones only hold the
/// functions that changed.
const FIRST_ARENA: usize = 128 << 20;
const MIN_ARENA: usize = 4 << 20;
/// Generous bytes of unoptimized code (and data) per VIR statement, to size later arenas.
const BYTES_PER_STATEMENT: usize = 512;

/// A compiled version: its module (owning the code), and by function index in the program the
/// address of the code it defines and of the trampoline it defines.
pub(crate) struct Version {
    pub module: JITModule,
    pub code: Vec<Option<usize>>,
    pub trampolines: Vec<Option<usize>>,
}

/// What a version is compiled from, besides the program.
pub(crate) struct Inputs<'a> {
    /// How the version declares each function (see [`DevFunction`]).
    pub names: &'a [DevFunction],
    /// Addresses of every reference this version does not define.
    pub imports: &'a [(String, usize)],
    /// The host's symbol table.
    pub runtime: &'a [(String, usize)],
    /// The session's first version (sizes its arena).
    pub first: bool,
    /// Describe the code to debuggers (`debug_info::jit`).
    pub debug_info: bool,
}

/// Compile `program` as described by `inputs`, appending the time of each step to `timings`.
pub(crate) fn compile(
    program: &vir::Program,
    inputs: &Inputs,
    timings: &mut Vec<(&'static str, Duration)>,
) -> Result<Version, String> {
    let Inputs {
        names,
        imports,
        runtime,
        first,
        debug_info,
    } = *inputs;
    let start = Instant::now();
    let isa = crate::isa::make_isa(&crate::host_triple(), false, true)?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, address) in runtime.iter().chain(imports) {
        builder.symbol(name.clone(), *address as *const u8);
    }
    #[cfg(unix)]
    builder.symbol_lookup_fn(Box::new(crate::c_symbols::lookup));
    builder.memory_provider(crate::jit_memory::arena(arena_size(program, names, first))?);
    let mut module = JITModule::new(builder);
    let built = build_module(&mut module, program, &Naming::Dev(names))?;
    #[cfg(all(windows, target_arch = "x86_64"))]
    let unwind = crate::unwind::jit_windows::JitUnwind::stage(&mut module, &built.unwind)?;
    timings.push(("compile", start.elapsed()));
    let start = Instant::now();
    module
        .finalize_definitions()
        .map_err(|e| format!("codegen: finalizing JIT code: {e}"))?;
    timings.push(("finalize", start.elapsed()));
    let start = Instant::now();
    #[cfg(all(windows, target_arch = "x86_64"))]
    unwind.register(&module)?;
    #[cfg(unix)]
    crate::unwind::jit_systemv::register(&module, &built.unwind)?;
    timings.push(("unwind", start.elapsed()));
    #[cfg(unix)]
    if debug_info {
        let start = Instant::now();
        let addresses: Vec<u64> = (built.lines.iter())
            .map(|f| module.get_finalized_function(f.id) as u64)
            .collect();
        crate::debug_info::jit::register(program, &built.lines, &addresses)?;
        timings.push(("debug info", start.elapsed()));
    }
    // Debuggers read JIT line tables on Unix only.
    #[cfg(not(unix))]
    let _ = debug_info;
    let address = |id| module.get_finalized_function(id) as usize;
    let code = built.funcs.iter().map(|id| id.map(address)).collect();
    let trampolines = names
        .iter()
        .zip(&built.references)
        .map(|(name, &id)| name.trampoline.as_ref().map(|_| address(id)))
        .collect();
    Ok(Version {
        module,
        code,
        trampolines,
    })
}

/// Arena size for a version: everything for the first, else sized by the defined functions.
fn arena_size(program: &vir::Program, names: &[DevFunction], first: bool) -> usize {
    if first {
        return FIRST_ARENA;
    }
    let statements: usize = program
        .funcs
        .iter()
        .zip(names)
        .filter(|(_, n)| n.code.is_some())
        .flat_map(|(f, _)| &f.blocks)
        .map(|b| b.stmts.len() + 1)
        .sum();
    let data: usize = program.statics.iter().map(|s| s.bytes.len() + 16).sum();
    (statements * BYTES_PER_STATEMENT + data * 2 + names.len() * 64).clamp(MIN_ARENA, FIRST_ARENA)
}
