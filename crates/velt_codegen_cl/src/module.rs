//! Whole-program translation into any `cranelift_module::Module`: declares functions, externs
//! and statics (with their address relocations), then translates and defines each function.
//! Shared by the object backend and the JIT, so both exercise exactly the same code path.
//!
//! A `velt dev` version ([`Naming::Dev`]) declares references and code under different names:
//! calls and addresses go to a function's trampoline, and only the functions that changed are
//! translated (dev/).

use cranelift_codegen::ir::UserFuncName;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::Context;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc};
use rustc_hash::FxHashMap;
use target_lexicon::BinaryFormat;
use velt_vir::vir;

use crate::abi::{make_signature, validate_aggregates};
use crate::cache::{compile_all, CacheStats, CodeCache};
use crate::debug_info::FunctionLines;
use crate::function::{translate_function, LibFunctions};
use crate::unwind::FunctionUnwind;
use crate::CodegenResult;

/// Result of `build_module`.
pub(crate) struct Built {
    /// Cranelift ids of the code of `program.funcs`, in order; `None` for functions this module
    /// does not define (dev versions).
    pub funcs: Vec<Option<FuncId>>,
    /// What references to each function resolve to (in a dev version: its trampoline, an
    /// import, or the code itself).
    pub references: Vec<FuncId>,
    /// Unwind info per defined function: SystemV CFI for ELF/Mach-O, Windows x64 for x86_64
    /// COFF (none for arm64 COFF yet).
    pub unwind: Vec<FunctionUnwind>,
    /// Line tables per defined function, when the program has source locations.
    pub lines: Vec<FunctionLines>,
    /// How many functions came from the cache.
    pub cache: CacheStats,
}

/// Module-level ids of the program's entities, indexed by VIR id.
pub(crate) struct Declarations {
    /// What calls and address constants of each function refer to.
    pub funcs: Vec<FuncId>,
    /// `None` for a SIMD operation (`velt_vir::simd`): emitted inline, so not a symbol (an
    /// import nothing defines must not reach the linker, even unreferenced).
    pub externs: Vec<Option<FuncId>>,
    pub statics: Vec<DataId>,
    /// The thread-local data object of each `ThreadLocal` constant, by its id, where the ISA
    /// has thread-local storage (tls.rs).
    pub tls: FxHashMap<u64, DataId>,
    pub call_conv: CallConv,
}

/// How `build_module` names and links the program's functions.
pub(crate) enum Naming<'a> {
    /// Every function is defined under its symbol with its VIR linkage (objects, tests).
    Program,
    /// A `velt dev` version: one entry per VIR function.
    Dev(&'a [DevFunction]),
}

/// How a `velt dev` version declares one function.
pub(crate) struct DevFunction {
    /// The name every call and address of the function resolves to: its trampoline, or (for
    /// code that is never swapped) the code itself. Imported unless this module defines it.
    pub reference: String,
    /// The reference is a trampoline this module defines from these bytes (no relocations).
    pub trampoline: Option<Vec<u8>>,
    /// This module defines the function's code under this name (equal to `reference` when the
    /// reference is the code itself).
    pub code: Option<String>,
}

/// Declare and define everything in `program` inside `module`.
pub(crate) fn build_module<M: Module>(
    module: &mut M,
    program: &vir::Program,
    naming: &Naming,
) -> CodegenResult<Built> {
    build_module_cached(module, program, naming, None)
}

/// [`build_module`], compiling the functions on every core and through `cache` (see `cache`).
pub(crate) fn build_module_cached<M: Module>(
    module: &mut M,
    program: &vir::Program,
    naming: &Naming,
    cache: Option<&dyn CodeCache>,
) -> CodegenResult<Built> {
    validate_aggregates(program)?;
    let (decls, defs) = declare_all(module, program, naming)?;

    // Translate every function (sequential: it declares into the module)...
    let mut libs = LibFunctions::default();
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut translated = Vec::new();
    for (i, func) in program.funcs.iter().enumerate() {
        let Some(id) = defs[i] else { continue };
        let mut ctx = module.make_context();
        ctx.func.signature = make_signature(decls.call_conv, &func.params, func.ret)?;
        ctx.func.name = UserFuncName::user(0, id.as_u32());
        let srclocs = translate_function(
            module,
            program,
            &decls,
            &mut libs,
            func,
            &mut ctx.func,
            &mut builder_ctx,
        )
        .map_err(|e| format!("codegen: in function `{}`: {e}", func.symbol))?;
        translated.push((id, func, ctx, srclocs));
    }
    // ...compile them all (in parallel, through the cache)...
    let symbols: Vec<&str> = translated.iter().map(|t| t.1.symbol.as_str()).collect();
    let mut ctxs: Vec<Context> = translated
        .iter_mut()
        .map(|t| take_context(&mut t.2))
        .collect();
    let stats = compile_all(module.isa(), &mut ctxs, &symbols, cache)?;
    // ...and define their code in order.
    let mut unwind = Vec::new();
    let mut lines = Vec::new();
    for ((id, func, _, srclocs), ctx) in translated.iter().zip(&ctxs) {
        define_compiled(module, *id, ctx, &func.symbol)?;
        unwind.extend(unwind_info(module, *id, ctx, &func.symbol)?);
        if !program.files.is_empty() {
            lines.extend(FunctionLines::new(*id, func, ctx, srclocs));
        }
    }
    Ok(Built {
        funcs: defs,
        references: decls.funcs,
        unwind,
        lines,
        cache: stats,
    })
}

/// The context in `slot`, leaving an empty one.
fn take_context(slot: &mut Context) -> Context {
    std::mem::replace(slot, Context::new())
}

/// Declarations of everything, plus the ids of the function code this module defines.
fn declare_all<M: Module>(
    module: &mut M,
    program: &vir::Program,
    naming: &Naming,
) -> CodegenResult<(Declarations, Vec<Option<FuncId>>)> {
    let call_conv = module.isa().default_call_conv();
    let (funcs, defs) = match naming {
        Naming::Program => {
            let funcs = declare_program_functions(module, program, call_conv)?;
            // An imported function's code is in another object (vir.rs invariant 11).
            let defs = (funcs.iter().zip(&program.funcs))
                .map(|(&id, f)| (f.linkage != vir::Linkage::Import).then_some(id))
                .collect();
            (funcs, defs)
        }
        Naming::Dev(names) => declare_dev_functions(module, program, call_conv, names)?,
    };
    let mut externs = Vec::with_capacity(program.externs.len());
    let (mut tls, mut tls_keys) = (FxHashMap::default(), FxHashMap::default());
    for e in &program.externs {
        // A thread-local value: inline on thread-local storage, or a function this module
        // defines on the operating system's thread keys (tls.rs).
        if let Some(call) = velt_vir::tls::TlsCall::of_symbol(&e.symbol) {
            let (params, ret) = call.signature();
            if e.params != params || e.ret != ret {
                bail!("ICE: `{}` declared with another signature", e.symbol);
            }
            if crate::tls::native(module) {
                crate::tls::data(module, &mut tls, call)?;
                externs.push(None);
                continue;
            }
            let sig = make_signature(call_conv, &e.params, e.ret)?;
            let id = declare_function(
                module,
                call_conv,
                &e.symbol,
                &e.params,
                e.ret,
                Linkage::Local,
            )?;
            // A `velt dev` version imports the session's key slot (dev/mod.rs), so every
            // version of the program uses one key per constant.
            let shared = matches!(naming, Naming::Dev(_));
            crate::tls::define_on_keys(module, call_conv, &mut tls_keys, call, id, sig, shared)?;
            externs.push(Some(id));
            continue;
        }
        // Operations emitted inline (function/simd.rs, function/atomic.rs): nothing to link.
        if velt_vir::simd::SimdCall::of_symbol(&e.symbol).is_some()
            || velt_vir::atomic::AtomicCall::of_symbol(&e.symbol).is_some()
        {
            externs.push(None);
            continue;
        }
        externs.push(Some(declare_function(
            module,
            call_conv,
            &e.symbol,
            &e.params,
            e.ret,
            Linkage::Import,
        )?));
    }
    let mut statics = Vec::with_capacity(program.statics.len());
    for (i, s) in program.statics.iter().enumerate() {
        // A static atomic's word, in a `velt dev` version: the session's (dev/mod.rs), so
        // every version changes the same one.
        let declared = match (s.mutable, naming) {
            (Some(id), Naming::Dev(_)) => {
                let sym = velt_vir::atomic::static_symbol(id);
                module.declare_data(&sym, Linkage::Import, true, false)
            }
            (mutable, _) => module.declare_anonymous_data(mutable.is_some(), false),
        };
        statics.push(declared.map_err(|e| format!("codegen: declaring static #{i}: {e}"))?);
    }
    let targets = RelocTargets {
        funcs: &funcs,
        externs: &externs,
        statics: &statics,
    };
    // Defined after all declarations: relocations may point at any function, extern or static.
    for (i, s) in program.statics.iter().enumerate() {
        if s.mutable.is_none() || !matches!(naming, Naming::Dev(_)) {
            define_static(module, &targets, i, s)?;
        }
    }
    let decls = Declarations {
        funcs,
        externs,
        statics,
        tls,
        call_conv,
    };
    Ok((decls, defs))
}

/// Every function under its symbol, with its VIR linkage (an imported one as an import).
fn declare_program_functions<M: Module>(
    module: &mut M,
    program: &vir::Program,
    call_conv: CallConv,
) -> CodegenResult<Vec<FuncId>> {
    let internal = internal_linkage(module, program);
    let mut funcs = Vec::with_capacity(program.funcs.len());
    for f in &program.funcs {
        let linkage = match f.linkage {
            vir::Linkage::Export => Linkage::Export,
            vir::Linkage::Internal => internal,
            vir::Linkage::Import => Linkage::Import,
        };
        funcs.push(declare_function(
            module, call_conv, &f.symbol, &f.params, f.ret, linkage,
        )?);
    }
    Ok(funcs)
}

/// A dev version's functions: (reference ids, code ids). Trampolines are defined right away.
fn declare_dev_functions<M: Module>(
    module: &mut M,
    program: &vir::Program,
    call_conv: CallConv,
    names: &[DevFunction],
) -> CodegenResult<(Vec<FuncId>, Vec<Option<FuncId>>)> {
    if names.len() != program.funcs.len() {
        bail!(
            "ICE: dev naming for {} of {} functions",
            names.len(),
            program.funcs.len()
        );
    }
    let mut refs = Vec::with_capacity(names.len());
    let mut defs = Vec::with_capacity(names.len());
    for (f, name) in program.funcs.iter().zip(names) {
        let code_is_reference = name.code.as_ref() == Some(&name.reference);
        let linkage = if name.trampoline.is_some() || code_is_reference {
            Linkage::Local
        } else {
            Linkage::Import
        };
        let sym = &name.reference;
        let reference = declare_function(module, call_conv, sym, &f.params, f.ret, linkage)?;
        if let Some(bytes) = &name.trampoline {
            module
                .define_function_bytes(reference, 16, bytes, &[])
                .map_err(|e| format!("codegen: trampoline `{sym}`: {e}"))?;
        }
        let code = match &name.code {
            Some(_) if code_is_reference => Some(reference),
            Some(code) => Some(declare_function(
                module,
                call_conv,
                code,
                &f.params,
                f.ret,
                Linkage::Local,
            )?),
            None => None,
        };
        refs.push(reference);
        defs.push(code);
    }
    Ok((refs, defs))
}

/// Linkage of `Linkage::Internal` functions. With debug info wanted (the VIR carries source
/// locations) on COFF they become external symbols: `link /DEBUG` only puts external symbols
/// into the PDB, and without them debuggers and profilers attribute every Velt function to the
/// nearest public one. ELF and Mach-O keep local symbols in their symbol tables anyway.
fn internal_linkage<M: Module>(module: &M, program: &vir::Program) -> Linkage {
    let coff = module.isa().triple().binary_format == BinaryFormat::Coff;
    if coff && !program.files.is_empty() {
        Linkage::Export
    } else {
        Linkage::Local
    }
}

fn declare_function<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    symbol: &str,
    params: &[vir::Ty],
    ret: vir::Ty,
    linkage: Linkage,
) -> CodegenResult<FuncId> {
    let sig = make_signature(call_conv, params, ret)
        .map_err(|e| format!("codegen: function `{symbol}`: {e}"))?;
    module
        .declare_function(symbol, linkage, &sig)
        .map_err(|e| format!("codegen: declaring function `{symbol}`: {e}"))
}

/// Module ids that static relocations may refer to.
struct RelocTargets<'a> {
    funcs: &'a [FuncId],
    externs: &'a [Option<FuncId>],
    statics: &'a [DataId],
}

/// Statics are anonymous data objects (no symbol clashes with user code), read-only except
/// static atomics' words. Relocated
/// slots become absolute 8-byte address relocations (Abs8 / ADDR64) against their targets.
fn define_static<M: Module>(
    module: &mut M,
    targets: &RelocTargets,
    index: usize,
    data: &vir::StaticData,
) -> CodegenResult<()> {
    let align = data.align.max(1);
    if !align.is_power_of_two() {
        bail!(
            "codegen: static #{index}: alignment {} is not a power of two",
            data.align
        );
    }
    // Zero-sized data objects are awkward in some object formats; pad to one byte.
    let bytes = if data.bytes.is_empty() {
        vec![0u8]
    } else {
        data.bytes.clone()
    };
    let mut desc = DataDescription::new();
    desc.define(bytes.into_boxed_slice());
    desc.set_align(u64::from(align));
    for (offset, target) in &data.relocs {
        if u64::from(*offset) + 8 > data.bytes.len() as u64 {
            bail!("codegen: static #{index}: relocation at {offset} is out of bounds");
        }
        write_reloc(module, targets, &mut desc, *offset, target)
            .map_err(|e| format!("codegen: static #{index}: relocation at {offset}: {e}"))?;
    }
    module
        .define_data(targets.statics[index], &desc)
        .map_err(|e| format!("codegen: defining static #{index}: {e}"))?;
    Ok(())
}

fn write_reloc<M: Module>(
    module: &mut M,
    targets: &RelocTargets,
    desc: &mut DataDescription,
    offset: u32,
    target: &vir::Const,
) -> CodegenResult<()> {
    let lookup = |ids: &[FuncId], id: u32, what: &str| {
        ids.get(id as usize)
            .copied()
            .ok_or_else(|| format!("unknown {what} #{id}"))
    };
    match target {
        vir::Const::Func(f) => {
            let func = module.declare_func_in_data(lookup(targets.funcs, f.0, "function")?, desc);
            desc.write_function_addr(offset, func);
        }
        vir::Const::Extern(e) => {
            let id = match targets.externs.get(e.0 as usize) {
                Some(Some(id)) => *id,
                Some(None) => bail!("the address of SIMD operation #{} is taken", e.0),
                None => bail!("unknown extern #{}", e.0),
            };
            let func = module.declare_func_in_data(id, desc);
            desc.write_function_addr(offset, func);
        }
        vir::Const::Static(s) => {
            let data = targets
                .statics
                .get(s.0 as usize)
                .copied()
                .ok_or_else(|| format!("unknown static #{}", s.0))?;
            let gv = module.declare_data_in_data(data, desc);
            desc.write_data_addr(offset, gv, 0);
        }
        other => bail!("target {other} is not an address"),
    }
    Ok(())
}

/// Compile `ctx`'s function and define it as `id` in `module`.
pub(crate) fn define<M: Module>(
    module: &mut M,
    id: FuncId,
    ctx: &mut Context,
    symbol: &str,
) -> CodegenResult<()> {
    crate::cache::compile_all(module.isa(), std::slice::from_mut(ctx), &[symbol], None)?;
    define_compiled(module, id, ctx, symbol)
}

/// Define `id` in `module` with the code compiled in `ctx`.
fn define_compiled<M: Module>(
    module: &mut M,
    id: FuncId,
    ctx: &Context,
    symbol: &str,
) -> CodegenResult<()> {
    let code = ctx
        .compiled_code()
        .ok_or_else(|| format!("ICE: function `{symbol}` was not compiled"))?;
    let relocs: Vec<ModuleReloc> = code
        .buffer
        .relocs()
        .iter()
        .map(|reloc| ModuleReloc::from_mach_reloc(reloc, &ctx.func, id))
        .collect();
    let alignment = u64::from(code.buffer.alignment);
    module
        .define_function_bytes(id, alignment, code.buffer.data(), &relocs)
        .map_err(|e| format!("codegen: defining function `{symbol}` failed: {e:?}"))
}

pub(crate) fn unwind_info<M: Module>(
    module: &M,
    id: FuncId,
    ctx: &Context,
    symbol: &str,
) -> CodegenResult<Option<FunctionUnwind>> {
    let Some(code) = ctx.compiled_code() else {
        return Ok(None);
    };
    let info = code
        .create_unwind_info(module.isa())
        .map_err(|e| format!("codegen: unwind info for `{symbol}`: {e:?}"))?;
    Ok(info.map(|info| (id, code.code_info().total_size, info)))
}
