//! Terminators: jumps, branches, switches, returns, calls (direct, extern, indirect), traps.

use std::collections::HashSet;

use cranelift_codegen::ir::{self, InstBuilder, TrapCode};
use cranelift_frontend::Switch;
use cranelift_module::Module;
use velt_vir::vir::{BlockId, Callee, Operand, Place, Terminator, Ty};

use super::{Translator, Val};
use crate::abi::{as_int, int_bits, make_signature};
use crate::CodegenResult;

impl<M: Module> Translator<'_, '_, M> {
    pub(super) fn terminator(&mut self, t: &Terminator) -> CodegenResult<()> {
        match t {
            Terminator::Goto(target) => {
                let block = self.block(*target)?;
                self.builder.ins().jump(block, &[]);
            }
            Terminator::Branch { cond, then, els } => self.branch(cond, *then, *els)?,
            Terminator::Switch {
                value,
                cases,
                default,
            } => self.switch(value, cases, *default)?,
            Terminator::Return(op) => self.ret(op)?,
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => self.call(callee, args, dest.as_ref(), *next)?,
            Terminator::Unreachable => {
                self.builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
        Ok(())
    }

    fn branch(&mut self, cond: &Operand, then: BlockId, els: BlockId) -> CodegenResult<()> {
        let (c, ty) = self.scalar(cond)?;
        if ty != Ty::Bool {
            bail!("branch condition must be Bool, found {ty:?}");
        }
        let (then, els) = (self.block(then)?, self.block(els)?);
        self.builder.ins().brif(c, then, &[], els, &[]);
        Ok(())
    }

    fn switch(
        &mut self,
        value: &Operand,
        cases: &[(i128, BlockId)],
        default: BlockId,
    ) -> CodegenResult<()> {
        let (v, ty) = self.scalar(value)?;
        let Some(int_ty) = as_int(ty) else {
            bail!("switch on non-integer type {ty:?}")
        };
        // `Switch` compares unsigned: map (possibly negative) case values to their bit
        // pattern at the value's width.
        let mask = (1u128 << int_bits(int_ty)) - 1;
        let mut switch = Switch::new();
        let mut seen = HashSet::new();
        for (k, target) in cases {
            let key = (*k as u128) & mask;
            if !seen.insert(key) {
                bail!("duplicate switch case {k}");
            }
            switch.set_entry(key, self.block(*target)?);
        }
        let default = self.block(default)?;
        switch.emit(&mut self.builder, v, default);
        Ok(())
    }

    fn ret(&mut self, op: &Operand) -> CodegenResult<()> {
        let ret = self.function.ret;
        if ret == Ty::Unit {
            self.builder.ins().return_(&[]);
            return Ok(());
        }
        let (v, ty) = self.scalar(op)?;
        if ty != ret {
            bail!("returning {ty:?} from a function returning {ret:?}");
        }
        self.builder.ins().return_(&[v]);
        Ok(())
    }

    fn call(
        &mut self,
        callee: &Callee,
        args: &[Operand],
        dest: Option<&Place>,
        next: BlockId,
    ) -> CodegenResult<()> {
        let (params, ret) = self.callee_signature(callee)?;
        let mut values = Vec::with_capacity(args.len());
        let mut types = Vec::with_capacity(args.len());
        for a in args {
            let (v, ty) = self.scalar(a)?;
            values.push(v);
            types.push(ty);
        }
        if params != types {
            bail!("call argument types {types:?} do not match parameters {params:?}");
        }
        let result = match self.inline_math(callee, &values, &params, ret) {
            Some(v) => Some(v),
            None => {
                let inst = self.emit_call(callee, &values)?;
                self.builder.inst_results(inst).first().copied()
            }
        };
        if let (Some(dest), Some(result)) = (dest, result) {
            let loc = self.place(dest)?;
            if loc.ty() != ret && loc.ty() != Ty::Unit {
                bail!(
                    "call returns {ret:?} but destination has type {:?}",
                    loc.ty()
                );
            }
            self.write(loc, Val::Scalar(result))?;
        }
        let next = self.block(next)?;
        self.builder.ins().jump(next, &[]);
        Ok(())
    }

    fn callee_signature(&self, callee: &Callee) -> CodegenResult<(Vec<Ty>, Ty)> {
        Ok(match callee {
            Callee::Func(id) => {
                self.func_id(*id)?;
                let f = self.program.func(*id);
                (f.params.clone(), f.ret)
            }
            Callee::Extern(id) => {
                self.extern_id(*id)?;
                let e = self.program.ext(*id);
                (e.params.clone(), e.ret)
            }
            Callee::Ptr { params, ret, .. } => (params.clone(), *ret),
        })
    }

    /// The runtime math functions that are exactly one Cranelift instruction
    /// (rt_abi_async.md §9) are emitted inline instead of called. `Math.round` is not: JS
    /// rounds ties toward +Infinity, unlike `nearest`.
    fn inline_math(
        &mut self,
        callee: &Callee,
        args: &[ir::Value],
        params: &[Ty],
        ret: Ty,
    ) -> Option<ir::Value> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        if params != [Ty::F64] || ret != Ty::F64 {
            return None;
        }
        let x = args[0];
        let ins = self.builder.ins();
        Some(
            match self.program.externs.get(id.0 as usize)?.symbol.as_str() {
                "velt_rt_math_sqrt" => ins.sqrt(x),
                "velt_rt_math_floor" => ins.floor(x),
                "velt_rt_math_ceil" => ins.ceil(x),
                "velt_rt_math_trunc" => ins.trunc(x),
                "velt_rt_math_fabs" => ins.fabs(x),
                _ => return None,
            },
        )
    }

    fn emit_call(&mut self, callee: &Callee, args: &[ir::Value]) -> CodegenResult<ir::Inst> {
        Ok(match callee {
            Callee::Func(id) => {
                let func = self.func_id(*id)?;
                let r = self.func_ref(func);
                self.builder.ins().call(r, args)
            }
            Callee::Extern(id) => {
                let func = self.extern_id(*id)?;
                let r = self.func_ref(func);
                self.builder.ins().call(r, args)
            }
            Callee::Ptr {
                target,
                params,
                ret,
            } => {
                let (target, ty) = self.scalar(target)?;
                if ty != Ty::Ptr {
                    bail!("indirect call target must be Ptr, found {ty:?}");
                }
                let sig = make_signature(self.decls.call_conv, params, *ret)?;
                let sig = self.builder.import_signature(sig);
                self.builder.ins().call_indirect(sig, target, args)
            }
        })
    }
}
