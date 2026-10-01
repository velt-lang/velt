//! One program version in its own `JITModule`: compiles the functions the version defines,
//! resolves everything else to code that is already loaded (trampolines, pinned code of earlier
//! versions, runtime symbols, C library functions), and registers the new code's unwind info
//! with the system (Windows x64, macOS, Linux).

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir;

use crate::module::{build_module, DevFunction, Naming};

/// Address space reserved for one version's code, data and unwind records on Windows x64,
/// where it is committed up front (charged against the page file): the first version gets the
/// most; later ones only hold the functions that changed.
#[cfg(all(windows, target_arch = "x86_64"))]
const FIRST_ARENA: usize = 128 << 20;
#[cfg(all(windows, target_arch = "x86_64"))]
const MIN_ARENA: usize = 4 << 20;
/// Generous bytes of unoptimized code (and data) per VIR statement, to size later arenas.
#[cfg(all(windows, target_arch = "x86_64"))]
const BYTES_PER_STATEMENT: usize = 512;

/// A compiled version: its module (owning the code), and by function index in the program the
/// address of the code it defines and of the trampoline it defines.
pub(crate) struct Version {
    pub module: JITModule,
    pub code: Vec<Option<usize>>,
    pub trampolines: Vec<Option<usize>>,
}

/// Compile `program` with `names` (see [`DevFunction`]). `imports` resolves every reference
/// this version does not define; `runtime` is the host's symbol table.
pub(crate) fn compile(
    program: &vir::Program,
    names: &[DevFunction],
    imports: &[(String, usize)],
    runtime: &[(String, usize)],
    first: bool,
) -> Result<Version, String> {
    let isa = crate::isa::make_isa(&crate::host_triple(), false, true)?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, address) in runtime.iter().chain(imports) {
        builder.symbol(name.clone(), *address as *const u8);
    }
    #[cfg(unix)]
    builder.symbol_lookup_fn(Box::new(crate::c_symbols::lookup));
    #[cfg(all(windows, target_arch = "x86_64"))]
    builder.memory_provider(crate::unwind::jit_windows::arena(arena_size(
        program, names, first,
    ))?);
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    let _ = first;
    let mut module = JITModule::new(builder);
    let built = build_module(&mut module, program, &Naming::Dev(names))?;
    #[cfg(all(windows, target_arch = "x86_64"))]
    let unwind = crate::unwind::jit_windows::JitUnwind::stage(&mut module, &built.unwind)?;
    module
        .finalize_definitions()
        .map_err(|e| format!("codegen: finalizing JIT code: {e}"))?;
    #[cfg(all(windows, target_arch = "x86_64"))]
    unwind.register(&module)?;
    #[cfg(unix)]
    crate::unwind::jit_systemv::register(&module, &built.unwind)?;
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
#[cfg(all(windows, target_arch = "x86_64"))]
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
