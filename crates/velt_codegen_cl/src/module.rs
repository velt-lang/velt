//! Whole-program translation into any `cranelift_module::Module`: declares functions, externs
//! and statics (with their address relocations), then translates and defines each function.
//! Shared by the object backend and the JIT, so both exercise exactly the same code path.
//!
//! A `velt dev` version ([`Naming::Dev`]) declares references and code under different names:
//! calls and addresses go to a function's trampoline, and only the functions that changed are
//! translated (dev/).

use cranelift_codegen::ir::UserFuncName;
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::{CodegenError, Context};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleError};
use target_lexicon::BinaryFormat;
use velt_vir::vir;

use crate::abi::{make_signature, validate_aggregates};
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
}

/// Module-level ids of the program's entities, indexed by VIR id.
pub(crate) struct Declarations {
    /// What calls and address constants of each function refer to.
    pub funcs: Vec<FuncId>,
    pub externs: Vec<FuncId>,
    pub statics: Vec<DataId>,
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
    validate_aggregates(program)?;
    let (decls, defs) = declare_all(module, program, naming)?;

    let mut unwind = Vec::new();
    let mut lines = Vec::new();
    let mut libs = LibFunctions::default();
    let mut ctx = module.make_context();
    let mut builder_ctx = FunctionBuilderContext::new();
    for (i, func) in program.funcs.iter().enumerate() {
        let Some(id) = defs[i] else { continue };
        module.clear_context(&mut ctx);
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
        define(module, id, &mut ctx, &func.symbol)?;
        unwind.extend(unwind_info(module, id, &ctx, &func.symbol)?);
        if !program.files.is_empty() {
            lines.extend(FunctionLines::new(id, func, &ctx, &srclocs));
        }
    }
    module.clear_context(&mut ctx);
    Ok(Built {
        funcs: defs,
        references: decls.funcs,
        unwind,
        lines,
    })
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
            let defs = funcs.iter().copied().map(Some).collect();
            (funcs, defs)
        }
        Naming::Dev(names) => declare_dev_functions(module, program, call_conv, names)?,
    };
    let mut externs = Vec::with_capacity(program.externs.len());
    for e in &program.externs {
        externs.push(declare_function(
            module,
            call_conv,
            &e.symbol,
            &e.params,
            e.ret,
            Linkage::Import,
        )?);
    }
    let mut statics = Vec::with_capacity(program.statics.len());
    for i in 0..program.statics.len() {
        statics.push(
            module
                .declare_anonymous_data(false, false)
                .map_err(|e| format!("codegen: declaring static #{i}: {e}"))?,
        );
    }
    let targets = RelocTargets {
        funcs: &funcs,
        externs: &externs,
        statics: &statics,
    };
    // Defined after all declarations: relocations may point at any function, extern or static.
    for (i, s) in program.statics.iter().enumerate() {
        define_static(module, &targets, i, s)?;
    }
    let decls = Declarations {
        funcs,
        externs,
        statics,
        call_conv,
    };
    Ok((decls, defs))
}

/// Every function under its symbol, with its VIR linkage.
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
    externs: &'a [FuncId],
    statics: &'a [DataId],
}

/// Statics are anonymous read-only data objects (no symbol clashes with user code). Relocated
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
            let func = module.declare_func_in_data(lookup(targets.externs, e.0, "extern")?, desc);
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

pub(crate) fn define<M: Module>(
    module: &mut M,
    id: FuncId,
    ctx: &mut Context,
    symbol: &str,
) -> CodegenResult<()> {
    match module.define_function(id, ctx) {
        Ok(()) => Ok(()),
        Err(ModuleError::Compilation(CodegenError::Verifier(errs))) => {
            let msg = cranelift_codegen::print_errors::pretty_verifier_error(&ctx.func, None, errs);
            bail!("codegen: compiling function `{symbol}` failed: {msg}")
        }
        Err(other) => bail!("codegen: compiling function `{symbol}` failed: {other:?}"),
    }
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
