//! The `velt_tls_*` externs (`velt_vir::tls`): a module constant `ThreadLocal<T>`'s value on each
//! thread.
//!
//! Where Cranelift has thread-local storage for the object format (`tls_model`: ELF, Mach-O, and
//! COFF on x86_64), each constant gets a thread-local data object `velt_tls_data_<id>` that
//! starts as the constant's starting value, and the calls are loads and stores through
//! `tls_value` (function/tls.rs). Elsewhere (the `velt dev` JIT, which `cranelift-jit` gives no
//! thread-local storage, and arm64 Windows) the module defines the externs itself as functions
//! on the operating system's thread keys: `pthread_getspecific` / `TlsGetValue` of a key made the
//! first time it is needed (`velt_tls_key_<id>` holds it plus one, 0 before). A key's value is
//! the word XOR the starting word, so a thread that never set it (null) reads the starting word.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, UserFuncName};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::TlsModel;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use rustc_hash::FxHashMap;
use target_lexicon::{BinaryFormat, OperatingSystem};
use velt_vir::tls::TlsCall;

use crate::CodegenResult;

/// Whether `module`'s ISA has thread-local storage (only object modules set a `tls_model`).
pub(crate) fn native<M: Module>(module: &M) -> bool {
    module.isa().flags().tls_model() != TlsModel::None
}

/// The thread-local data object of `call`'s constant, declared and defined on first use.
pub(crate) fn data<M: Module>(
    module: &mut M,
    datas: &mut FxHashMap<u32, DataId>,
    call: TlsCall,
) -> CodegenResult<DataId> {
    if let Some(d) = datas.get(&call.id()) {
        return Ok(*d);
    }
    let sym = call.data_symbol();
    let id = module
        .declare_data(&sym, Linkage::Local, true, true)
        .map_err(|e| format!("codegen: declaring `{sym}`: {e}"))?;
    let n = (call.bits() / 8) as usize;
    let mut desc = DataDescription::new();
    desc.define(call.init().to_le_bytes()[..n].to_vec().into_boxed_slice());
    desc.set_align(n as u64);
    module
        .define_data(id, &desc)
        .map_err(|e| format!("codegen: defining `{sym}`: {e}"))?;
    datas.insert(call.id(), id);
    Ok(id)
}

/// Defines the extern `call` (symbol `symbol`, signature `sig`) as a function on the operating
/// system's thread keys, for a module without thread-local storage.
pub(crate) fn define_on_keys<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    keys: &mut FxHashMap<u32, DataId>,
    call: TlsCall,
    func: FuncId,
    sig: ir::Signature,
) -> CodegenResult<()> {
    let windows = module.isa().triple().operating_system == OperatingSystem::Windows;
    let key = match keys.get(&call.id()) {
        Some(k) => *k,
        None => {
            let sym = format!("velt_tls_key_{}", call.id());
            let k = module
                .declare_data(&sym, Linkage::Local, true, false)
                .map_err(|e| format!("codegen: declaring `{sym}`: {e}"))?;
            let mut desc = DataDescription::new();
            desc.define_zeroinit(8);
            desc.set_align(8);
            module
                .define_data(k, &desc)
                .map_err(|e| format!("codegen: defining `{sym}`: {e}"))?;
            keys.insert(call.id(), k);
            k
        }
    };
    let os = OsKeys::declare(module, call_conv, windows)?;
    let mut ctx = module.make_context();
    ctx.func.signature = sig;
    ctx.func.name = UserFuncName::user(0, func.as_u32());
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let slot_gv = module.declare_data_in_func(key, b.func);
    // An absolute address: cranelift-object cannot emit ADRP relocations for arm64 COFF (as
    // `far_addresses` in function/).
    if module.isa().triple().binary_format == BinaryFormat::Coff {
        if let ir::GlobalValueData::Symbol { colocated, .. } = &mut b.func.global_values[slot_gv] {
            *colocated = false;
        }
    }
    let slot = b.ins().symbol_value(types::I64, slot_gv);
    let flags = MemFlags::trusted();
    // The key plus one, made now if no thread made it yet.
    let have = b.ins().atomic_load(types::I64, flags, slot);
    let make = b.create_block();
    let ready = b.create_block();
    b.append_block_param(ready, types::I64);
    b.ins().brif(have, ready, &[have.into()], make, &[]);
    b.switch_to_block(make);
    let made = os.make_key(module, &mut b, slot)?;
    b.ins().jump(ready, &[made.into()]);
    b.switch_to_block(ready);
    let k1 = b.block_params(ready)[0];
    let k = b.ins().iadd_imm(k1, -1);
    let width = types::Type::int(call.bits() as u16).unwrap_or(types::I8);
    let init = b.ins().iconst(types::I64, call.init() as i64);
    match call {
        TlsCall::Get { .. } => {
            let p = os.get(module, &mut b, k)?;
            let w = b.ins().bxor(p, init);
            let w = match width {
                types::I64 => w,
                t => b.ins().ireduce(t, w),
            };
            b.ins().return_(&[w]);
        }
        TlsCall::Set { .. } => {
            let v = b.block_params(entry)[0];
            let v = match width {
                types::I64 => v,
                _ => b.ins().uextend(types::I64, v),
            };
            let p = b.ins().bxor(v, init);
            os.set(module, &mut b, k, p)?;
            b.ins().return_(&[]);
        }
    }
    b.seal_all_blocks();
    b.finalize();
    module
        .define_function(func, &mut ctx)
        .map_err(|e| format!("codegen: defining `{}`: {e}", call.symbol()))
}

/// The operating system's thread-key functions, imported.
struct OsKeys {
    windows: bool,
    create: FuncId,
    delete: FuncId,
    get: FuncId,
    set: FuncId,
}

impl OsKeys {
    fn declare<M: Module>(module: &mut M, cc: CallConv, windows: bool) -> CodegenResult<OsKeys> {
        let i64_ = AbiParam::new(types::I64);
        let i32_ = AbiParam::new(types::I32);
        let mut decl = |name: &str, params: &[AbiParam], ret: &[AbiParam]| {
            let mut sig = ir::Signature::new(cc);
            sig.params.extend_from_slice(params);
            sig.returns.extend_from_slice(ret);
            module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|e| format!("codegen: declaring `{name}`: {e}"))
        };
        Ok(if windows {
            // DWORD TlsAlloc(void); BOOL TlsFree(DWORD); LPVOID TlsGetValue(DWORD);
            // BOOL TlsSetValue(DWORD, LPVOID).
            OsKeys {
                windows,
                create: decl("TlsAlloc", &[], &[i32_])?,
                delete: decl("TlsFree", &[i32_], &[i32_])?,
                get: decl("TlsGetValue", &[i32_], &[i64_])?,
                set: decl("TlsSetValue", &[i32_, i64_], &[i32_])?,
            }
        } else {
            // int pthread_key_create(pthread_key_t *, void (*)(void *)); int
            // pthread_key_delete(pthread_key_t); void *pthread_getspecific(pthread_key_t); int
            // pthread_setspecific(pthread_key_t, const void *). A key is an `unsigned long` on
            // macOS and an `unsigned int` elsewhere: passed in a 64-bit register either way.
            OsKeys {
                windows,
                create: decl("pthread_key_create", &[i64_, i64_], &[i32_])?,
                delete: decl("pthread_key_delete", &[i64_], &[i32_])?,
                get: decl("pthread_getspecific", &[i64_], &[i64_])?,
                set: decl("pthread_setspecific", &[i64_, i64_], &[i32_])?,
            }
        })
    }

    fn call<M: Module>(
        module: &mut M,
        b: &mut FunctionBuilder,
        f: FuncId,
        args: &[ir::Value],
    ) -> Option<ir::Value> {
        let r = module.declare_func_in_func(f, b.func);
        let inst = b.ins().call(r, args);
        b.inst_results(inst).first().copied()
    }

    /// The key of a 64-bit `k` (a Windows index is 32 bits).
    fn arg(&self, b: &mut FunctionBuilder, k: ir::Value) -> ir::Value {
        match self.windows {
            true => b.ins().ireduce(types::I32, k),
            false => k,
        }
    }

    /// Makes a key and installs it plus one at `slot`, unless another thread did first (then
    /// this one is deleted); the key plus one that is installed.
    fn make_key<M: Module>(
        &self,
        module: &mut M,
        b: &mut FunctionBuilder,
        slot: ir::Value,
    ) -> CodegenResult<ir::Value> {
        let key = if self.windows {
            let idx = Self::call(module, b, self.create, &[]).ok_or("ICE: TlsAlloc")?;
            b.ins().uextend(types::I64, idx)
        } else {
            let ss = b.create_sized_stack_slot(ir::StackSlotData::new(
                ir::StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let zero = b.ins().iconst(types::I64, 0);
            b.ins().stack_store(zero, ss, 0);
            let at = b.ins().stack_addr(types::I64, ss, 0);
            Self::call(module, b, self.create, &[at, zero]);
            let unsigned_long = matches!(
                module.isa().triple().operating_system,
                OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_)
            );
            match unsigned_long {
                true => b.ins().stack_load(types::I64, ss, 0),
                false => {
                    let k = b.ins().stack_load(types::I32, ss, 0);
                    b.ins().uextend(types::I64, k)
                }
            }
        };
        let k1 = b.ins().iadd_imm(key, 1);
        let zero = b.ins().iconst(types::I64, 0);
        let old = b.ins().atomic_cas(MemFlags::trusted(), slot, zero, k1);
        let lost = b.ins().icmp_imm(IntCC::NotEqual, old, 0);
        let undo = b.create_block();
        let done = b.create_block();
        b.append_block_param(done, types::I64);
        b.ins().brif(lost, undo, &[], done, &[k1.into()]);
        b.switch_to_block(undo);
        let k = self.arg(b, key);
        Self::call(module, b, self.delete, &[k]);
        b.ins().jump(done, &[old.into()]);
        b.switch_to_block(done);
        Ok(b.block_params(done)[0])
    }

    fn get<M: Module>(
        &self,
        module: &mut M,
        b: &mut FunctionBuilder,
        k: ir::Value,
    ) -> CodegenResult<ir::Value> {
        let k = self.arg(b, k);
        Self::call(module, b, self.get, &[k]).ok_or_else(|| "ICE: thread key read".into())
    }

    fn set<M: Module>(
        &self,
        module: &mut M,
        b: &mut FunctionBuilder,
        k: ir::Value,
        v: ir::Value,
    ) -> CodegenResult<()> {
        let k = self.arg(b, k);
        Self::call(module, b, self.set, &[k, v]);
        Ok(())
    }
}
