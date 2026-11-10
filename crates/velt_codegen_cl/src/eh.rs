//! `velt_eh_try` and its personality (`velt_vir::eh`, docs/internals/design/unwinding.md),
//! defined in each object whose program calls `velt_eh_try`, local to it.
//!
//! In an object for a target with the Itanium unwinder, `velt_eh_try` calls `code(env)` with a
//! `try_call_indirect` whose catch-all handler returns the exception object (the first payload
//! register). Cranelift records the handler's offset; this module writes the GCC LSDA for it
//! (one call-site entry covering the whole function, whose only call is the `try_call`) and
//! defines the personality routine, which claims only Velt's class and lands at the handler
//! itself (the frame has one call, so it needs no call-site lookup). The `.eh_frame` writer gives the
//! function's FDE a CIE naming them (unwind/eh_frame.rs). Elsewhere (the JIT, WebAssembly,
//! Windows) `velt_eh_try` is a plain call that returns null.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    self, types, AbiParam, BlockArg, BlockCall, ExceptionTableData, ExceptionTableItem,
    InstBuilder, UserFuncName,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::FinalizedMachExceptionHandler;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use target_lexicon::BinaryFormat;
use velt_vir::eh::{self, Catching};

use crate::unwind::FunctionUnwind;
use crate::CodegenResult;

/// The unwinder's answers (`_Unwind_Reason_Code`) and phase flags (`_Unwind_Action`).
const HANDLER_FOUND: i64 = 6;
const INSTALL_CONTEXT: i64 = 7;
const CONTINUE_UNWIND: i64 = 8;
const SEARCH_PHASE: i64 = 1;
const HANDLER_FRAME: i64 = 4;

/// What an object holds for catching, for its `.eh_frame`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EhSymbols {
    /// `velt_eh_try`, whose FDE names the personality and the LSDA.
    pub try_fn: FuncId,
    /// The personality routine.
    pub personality: FuncId,
    /// A pointer to it, which an ELF CIE names (indirectly, as compilers do); a Mach-O CIE
    /// names the routine through the GOT, the only form ld64 accepts.
    pub personality_ref: DataId,
    /// The LSDA of `velt_eh_try`.
    pub lsda: DataId,
}

/// `velt_eh_try` (declared as `try_fn`) and, where it catches, the personality and LSDA:
/// their unwind info goes to `unwind`.
pub(crate) fn define<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    try_fn: FuncId,
    unwind: &mut Vec<FunctionUnwind>,
) -> CodegenResult<Option<EhSymbols>> {
    let catching = catches(module);
    let sig = crate::abi::make_signature(
        call_conv,
        &[velt_vir::vir::Ty::Ptr; 2],
        velt_vir::vir::Ty::Ptr,
    )?;
    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    ctx.func.name = UserFuncName::user(0, try_fn.as_u32());
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let (code, env) = (b.block_params(entry)[0], b.block_params(entry)[1]);
    // The callee's convention for the exceptional edge: SystemV, whose payload registers and
    // restored callee-saved registers are the Itanium unwinder's (Cranelift implements no other
    // for `try_call`; for one pointer argument and no result it is the platform's C convention,
    // AppleAarch64 included).
    let mut callee = ir::Signature::new(if catching {
        CallConv::SystemV
    } else {
        call_conv
    });
    callee.params.push(AbiParam::new(types::I64));
    let callee = b.import_signature(callee);
    if catching {
        let ok = b.create_block();
        let pad = b.create_block();
        b.append_block_param(pad, types::I64);
        let pool = &mut b.func.dfg.value_lists;
        let normal = BlockCall::new(ok, [], pool);
        let caught = BlockCall::new(pad, [BlockArg::TryCallExn(0)], pool);
        let table = ExceptionTableData::new(callee, normal, [ExceptionTableItem::Default(caught)]);
        let table = b.func.dfg.exception_tables.push(table);
        b.ins().try_call_indirect(code, &[env], table);
        b.switch_to_block(ok);
        let null = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[null]);
        b.switch_to_block(pad);
        let e = b.block_params(pad)[0];
        b.ins().return_(&[e]);
    } else {
        b.ins().call_indirect(callee, code, &[env]);
        let null = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[null]);
    }
    b.seal_all_blocks();
    b.finalize();
    module
        .define_function(try_fn, &mut ctx)
        .map_err(|e| format!("codegen: defining `{}`: {e:?}", eh::TRY))?;
    unwind.extend(crate::module::unwind_info(
        module.isa(),
        try_fn,
        &ctx,
        eh::TRY,
    )?);
    if !catching {
        return Ok(None);
    }
    let code = ctx
        .compiled_code()
        .ok_or_else(|| format!("ICE: `{}` not compiled", eh::TRY))?;
    let len = code.code_info().total_size;
    let pad = code
        .buffer
        .call_sites()
        .flat_map(|site| site.exception_handlers.iter())
        .find_map(|h| match h {
            FinalizedMachExceptionHandler::Default(at) => Some(*at),
            _ => None,
        })
        .ok_or_else(|| format!("ICE: `{}` has no landing pad", eh::TRY))?;
    let personality = define_personality(module, call_conv, pad, unwind)?;
    let personality_ref = data(module, "velt_eh_personality_ref", true, |m, desc| {
        desc.define(vec![0; 8].into_boxed_slice());
        desc.set_align(8);
        let f = m.declare_func_in_data(personality, desc);
        desc.write_function_addr(0, f);
    })?;
    let section = match module.isa().triple().binary_format {
        BinaryFormat::Macho => ("__TEXT", "__gcc_except_tab"),
        _ => ("", ".gcc_except_table"),
    };
    let lsda = data(module, "velt_eh_try_lsda", false, |_, desc| {
        desc.define(lsda(len, pad).into_boxed_slice());
        desc.set_align(4);
        desc.set_segment_section(section.0, section.1, 0);
    })?;
    Ok(Some(EhSymbols {
        try_fn,
        personality,
        personality_ref,
        lsda,
    }))
}

/// Whether `module`'s `velt_eh_try` catches (see [`define`]): an Itanium target and an object
/// module (only object modules set a TLS model: the JIT registers no LSDA, crate::tls::native).
fn catches<M: Module>(module: &M) -> bool {
    let triple = module.isa().triple().to_string();
    Catching::of_triple(&triple) == Catching::Itanium && crate::tls::native(module)
}

/// `velt_eh_catches` (declared as `id`): returns 1 where this module's `velt_eh_try` catches, else
/// 0; its unwind info goes to `unwind`.
pub(crate) fn define_catches<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    id: FuncId,
    unwind: &mut Vec<FunctionUnwind>,
) -> CodegenResult<()> {
    let (params, ret) = eh::catches_signature();
    let mut ctx = module.make_context();
    ctx.func.signature = crate::abi::make_signature(call_conv, &params, ret)?;
    ctx.func.name = UserFuncName::user(0, id.as_u32());
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let entry = b.create_block();
    b.switch_to_block(entry);
    let v = b.ins().iconst(types::I8, i64::from(catches(module)));
    b.ins().return_(&[v]);
    b.seal_all_blocks();
    b.finalize();
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("codegen: defining `{}`: {e:?}", eh::CATCHES))?;
    unwind.extend(crate::module::unwind_info(
        module.isa(),
        id,
        &ctx,
        eh::CATCHES,
    )?);
    Ok(())
}

/// A local data object `name`, filled by `fill`.
fn data<M: Module>(
    module: &mut M,
    name: &str,
    writable: bool,
    fill: impl FnOnce(&mut M, &mut DataDescription),
) -> CodegenResult<DataId> {
    let id = module
        .declare_data(name, Linkage::Local, writable, false)
        .map_err(|e| format!("codegen: declaring `{name}`: {e}"))?;
    let mut desc = DataDescription::new();
    fill(module, &mut desc);
    module
        .define_data(id, &desc)
        .map_err(|e| format!("codegen: defining `{name}`: {e}"))?;
    Ok(id)
}

/// `velt_eh_personality(version, actions, class, exception, context)`, the routine of
/// `velt_eh_try`'s frame only: `_URC_CONTINUE_UNWIND` for an exception of another class (both
/// phases); for Velt's, `_URC_HANDLER_FOUND` in the search phase and, in the cleanup phase at this
/// frame, the landing pad (`pad` bytes into the function, which `_Unwind_GetRegionStart` gives)
/// with the exception in data register 0 and selector 0 in register 1 (DWARF 0 and 1: rax/rdx,
/// x0/x1), `_URC_INSTALL_CONTEXT`. The frame's one call needs no call-site lookup. Only
/// libunwind / libgcc_s functions are called: no runtime.
fn define_personality<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    pad: u32,
    unwind: &mut Vec<FunctionUnwind>,
) -> CodegenResult<FuncId> {
    let sig_of = |params: &[ir::Type], ret: Option<ir::Type>| {
        let mut sig = ir::Signature::new(call_conv);
        sig.params.extend(params.iter().map(|&t| AbiParam::new(t)));
        sig.returns.extend(ret.map(AbiParam::new));
        sig
    };
    let (i32_, i64_) = (types::I32, types::I64);
    let sig = sig_of(&[i32_, i32_, i64_, i64_, i64_], Some(i32_));
    let declare = |m: &mut M, name: &str, linkage, sig: &ir::Signature| {
        m.declare_function(name, linkage, sig)
            .map_err(|e| format!("codegen: declaring `{name}`: {e}"))
    };
    let id = declare(module, eh::PERSONALITY, Linkage::Local, &sig)?;
    let region = sig_of(&[i64_], Some(i64_));
    let region = declare(module, "_Unwind_GetRegionStart", Linkage::Import, &region)?;
    let set_gr = sig_of(&[i64_, i32_, i64_], None);
    let set_gr = declare(module, "_Unwind_SetGR", Linkage::Import, &set_gr)?;
    let set_ip = sig_of(&[i64_, i64_], None);
    let set_ip = declare(module, "_Unwind_SetIP", Linkage::Import, &set_ip)?;
    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    ctx.func.name = UserFuncName::user(0, id.as_u32());
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let args = b.block_params(entry).to_vec();
    let (actions, exception, context) = (args[1], args[3], args[4]);
    let result = |b: &mut FunctionBuilder, code: i64| {
        let v = b.ins().iconst(i32_, code);
        b.ins().return_(&[v]);
    };
    let [pass, mine, found, cleanup, land] = [(); 5].map(|_| b.create_block());
    let ours = b.ins().icmp_imm(IntCC::Equal, args[2], eh::CLASS as i64);
    b.ins().brif(ours, mine, &[], pass, &[]);
    b.switch_to_block(pass);
    result(&mut b, CONTINUE_UNWIND);
    b.switch_to_block(mine);
    let search = b.ins().band_imm(actions, SEARCH_PHASE);
    b.ins().brif(search, found, &[], cleanup, &[]);
    b.switch_to_block(found);
    result(&mut b, HANDLER_FOUND);
    b.switch_to_block(cleanup);
    let here = b.ins().band_imm(actions, HANDLER_FRAME);
    b.ins().brif(here, land, &[], pass, &[]);
    b.switch_to_block(land);
    let region = module.declare_func_in_func(region, b.func);
    let set_gr = module.declare_func_in_func(set_gr, b.func);
    let set_ip = module.declare_func_in_func(set_ip, b.func);
    let call = b.ins().call(region, &[context]);
    let start = b.inst_results(call)[0];
    let target = b.ins().iadd_imm(start, i64::from(pad));
    let (r0, r1, zero) = (
        b.ins().iconst(i32_, 0),
        b.ins().iconst(i32_, 1),
        b.ins().iconst(i64_, 0),
    );
    b.ins().call(set_gr, &[context, r0, exception]);
    b.ins().call(set_gr, &[context, r1, zero]);
    b.ins().call(set_ip, &[context, target]);
    result(&mut b, INSTALL_CONTEXT);
    b.seal_all_blocks();
    b.finalize();
    module
        .define_function(id, &mut ctx)
        .map_err(|e| format!("codegen: defining `{}`: {e:?}", eh::PERSONALITY))?;
    unwind.extend(crate::module::unwind_info(
        module.isa(),
        id,
        &ctx,
        eh::PERSONALITY,
    )?);
    Ok(id)
}

/// The GCC LSDA of a function of `len` bytes whose every call lands at `pad` with a catch-all
/// clause: no landing-pad base, a type table of one entry (0: any exception, `udata4`), call
/// sites in ULEB128, one entry for the whole function, one action record (type 1, no next).
pub(crate) fn lsda(len: u32, pad: u32) -> Vec<u8> {
    let mut sites = vec![];
    for v in [0, len, pad, 1] {
        uleb(&mut sites, v);
    }
    let mut rest = vec![0x01]; // call sites: DW_EH_PE_uleb128
    uleb(&mut rest, sites.len() as u32);
    rest.extend(sites);
    rest.extend([0x01, 0x00]); // the action: type filter 1, no next action
    rest.extend([0, 0, 0, 0]); // type 1: null, catch-all
    let mut out = vec![0xff, 0x03]; // no landing-pad base; types: DW_EH_PE_udata4
    uleb(&mut out, rest.len() as u32); // from here to the end of the type table
    out.extend(rest);
    out
}

fn uleb(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsda_of_a_small_function() {
        // 40 bytes, landing pad at 24: header, 4 call-site bytes, action, type entry.
        assert_eq!(
            lsda(40, 24),
            vec![0xff, 0x03, 12, 0x01, 4, 0, 40, 24, 1, 0x01, 0x00, 0, 0, 0, 0]
        );
    }

    #[test]
    fn lsda_with_long_offsets() {
        let l = lsda(300, 200);
        // 300 and 200 take two ULEB128 bytes each.
        assert_eq!(&l[3..11], &[0x01, 6, 0, 0xac, 0x02, 0xc8, 0x01, 1]);
        assert_eq!(l[2] as usize, l.len() - 3);
    }
}
