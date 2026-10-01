//! The entry object of an executable linked against the shared runtime (`velt_rt_shared`): a C
//! `main(argc, argv)` that returns `velt_rt_start(argc, argv, velt_main)`. The static runtime
//! defines `main` itself, but a shared library cannot: it would leave `velt_main` undefined,
//! which DLLs and macOS dylibs do not allow.

use cranelift_codegen::ir::{types, AbiParam, InstBuilder, Signature, UserFuncName};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

use crate::module::{define, unwind_info};
use crate::{isa, unwind, CodegenResult};

/// Emit the entry object for `target` (an object file of the target's format).
pub(crate) fn emit_entry_object(target: &str) -> CodegenResult<Vec<u8>> {
    let isa = isa::make_isa(target, false, false)?;
    let builder = ObjectBuilder::new(
        isa.clone(),
        "velt_entry",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| format!("codegen: cannot create object builder: {e}"))?;
    let mut module = ObjectModule::new(builder);
    let ptr = module.target_config().pointer_type();
    let call_conv = module.isa().default_call_conv();

    // int main(int argc, char **argv)
    let mut main_sig = Signature::new(call_conv);
    main_sig
        .params
        .extend([AbiParam::new(types::I32), AbiParam::new(ptr)]);
    main_sig.returns.push(AbiParam::new(types::I32));
    // int32_t velt_main(void)
    let mut velt_main_sig = Signature::new(call_conv);
    velt_main_sig.returns.push(AbiParam::new(types::I32));
    // int velt_rt_start(int argc, char **argv, int32_t (*entry)(void))
    let mut start_sig = main_sig.clone();
    start_sig.params.push(AbiParam::new(ptr));

    let declare = |module: &mut ObjectModule, name: &str, linkage, sig: &Signature| {
        module
            .declare_function(name, linkage, sig)
            .map_err(|e| format!("codegen: declaring `{name}`: {e}"))
    };
    let main = declare(&mut module, "main", Linkage::Export, &main_sig)?;
    let velt_main = declare(&mut module, "velt_main", Linkage::Import, &velt_main_sig)?;
    let start = declare(&mut module, "velt_rt_start", Linkage::Import, &start_sig)?;

    let mut ctx = module.make_context();
    ctx.func.signature = main_sig;
    ctx.func.name = UserFuncName::user(0, main.as_u32());
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    let block = b.create_block();
    b.append_block_params_for_function_params(block);
    b.switch_to_block(block);
    b.seal_block(block);
    let (argc, argv) = (b.block_params(block)[0], b.block_params(block)[1]);
    let entry_ref = module.declare_func_in_func(velt_main, b.func);
    let start_ref = module.declare_func_in_func(start, b.func);
    let entry = b.ins().func_addr(ptr, entry_ref);
    let call = b.ins().call(start_ref, &[argc, argv, entry]);
    let code = b.inst_results(call)[0];
    b.ins().return_(&[code]);
    b.finalize();

    define(&mut module, main, &mut ctx, "main")?;
    let unwind = unwind_info(&module, main, &ctx, "main")?;
    let mut product = module.finish();
    unwind::add_unwind_info(&mut product, &*isa, &Vec::from_iter(unwind))?;
    product
        .emit()
        .map_err(|e| format!("codegen: cannot write the entry object: {e}"))
}
