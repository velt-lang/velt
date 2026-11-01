//! The `velt_tls_*` externs (`velt_vir::tls`) inline, where the ISA has thread-local storage: a
//! load or store of the constant's thread-local data object (crate::tls). Without it, the module
//! defines them as functions instead and calls stay calls.

use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags};
use cranelift_module::Module;
use velt_vir::tls::TlsCall;
use velt_vir::vir::Callee;

use super::simd::Inlined;
use super::Translator;

impl<M: Module> Translator<'_, '_, M> {
    /// Emit the thread-local extern `callee` inline, if it is one and the module has its data.
    pub(super) fn inline_tls(&mut self, callee: &Callee, values: &[ir::Value]) -> Option<Inlined> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let call = TlsCall::of_symbol(&self.program.externs.get(id.0 as usize)?.symbol)?;
        let data = *self.decls.tls.get(&call.id())?;
        let gv = match self.data_refs.get(&data) {
            Some(g) => *g,
            None => {
                let g = self.module.declare_data_in_func(data, self.builder.func);
                self.data_refs.insert(data, g);
                g
            }
        };
        let at = self.builder.ins().tls_value(types::I64, gv);
        let ty = types::Type::int(call.bits() as u16).unwrap_or(types::I8);
        let flags = MemFlags::trusted();
        Some(match call {
            TlsCall::Get { .. } => Inlined::Value(self.builder.ins().load(ty, flags, at, 0)),
            TlsCall::Set { .. } => {
                self.builder.ins().store(flags, values[0], at, 0);
                Inlined::Unit
            }
        })
    }
}
