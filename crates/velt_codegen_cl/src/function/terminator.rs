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
        let inline = match self.int32_extern(callee, &params, ret) {
            // ToInt32(x); the slow path calls the runtime with the same arguments.
            Some("velt_rt_math_to_int32") => Some(self.inline_to_int32(callee, values[0], &values)?),
            // ToInt32(a + x), always through the double sum (`velt_rt_math_add_int32`).
            Some("velt_rt_math_add_int32") => {
                let af = self.builder.ins().fcvt_from_sint(ir::types::F64, values[0]);
                let sum = self.builder.ins().fadd(af, values[1]);
                Some(self.inline_to_int32(callee, sum, &values)?)
            }
            _ => self.inline_math(callee, &values, &params, ret),
        };
        let result = match inline {
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
    /// (rt_abi_async.md §9, `Math.clz32`, JS's int32 multiply) are emitted inline instead of
    /// called. `Math.round` is not: JS
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
        let symbol = self.program.externs.get(id.0 as usize)?.symbol.as_str();
        if symbol == "velt_rt_math_clz32" && params == [Ty::I32] && ret == Ty::I32 {
            return Some(self.builder.ins().clz(args[0]));
        }
        if symbol == "velt_rt_math_mul_int32" && params == [Ty::I32, Ty::I32] && ret == Ty::I32 {
            // The exact product, rounded through a double like JS's multiply (|p| < 2^62).
            use ir::types::{F64, I32, I64};
            let x = self.builder.ins().sextend(I64, args[0]);
            let y = self.builder.ins().sextend(I64, args[1]);
            let p = self.builder.ins().imul(x, y);
            let d = self.builder.ins().fcvt_from_sint(F64, p);
            let t = self.builder.ins().fcvt_to_sint_sat(I64, d);
            return Some(self.builder.ins().ireduce(I32, t));
        }
        if params != [Ty::F64] || ret != Ty::F64 {
            return None;
        }
        let x = args[0];
        let ins = self.builder.ins();
        Some(
            match symbol {
                "velt_rt_math_sqrt" => ins.sqrt(x),
                "velt_rt_math_floor" => ins.floor(x),
                "velt_rt_math_ceil" => ins.ceil(x),
                "velt_rt_math_trunc" => ins.trunc(x),
                "velt_rt_math_fabs" => ins.fabs(x),
                _ => return None,
            },
        )
    }

    /// JS ToInt32 of `x` (`velt_rt_math_to_int32`): for |x| < 2^63 a conversion to `i64` and a
    /// truncation, which is exact (ToInt32 is the value modulo 2^32); NaN, ±Infinity and larger
    /// values call the runtime from a cold block.
    fn inline_to_int32(
        &mut self,
        callee: &Callee,
        x: ir::Value,
        args: &[ir::Value],
    ) -> CodegenResult<ir::Value> {
        use ir::condcodes::FloatCC;
        let fast = self.builder.create_block();
        let slow = self.builder.create_block();
        let done = self.builder.create_block();
        let r = self.builder.append_block_param(done, ir::types::I32);
        self.builder.set_cold_block(slow);
        let lo = self.builder.ins().f64const(-9_223_372_036_854_775_808.0);
        let hi = self.builder.ins().f64const(9_223_372_036_854_775_808.0);
        let ge = self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, x, lo);
        let lt = self.builder.ins().fcmp(FloatCC::LessThan, x, hi);
        let inside = self.builder.ins().band(ge, lt);
        self.builder.ins().brif(inside, fast, &[], slow, &[]);
        self.builder.switch_to_block(fast);
        let t = self.builder.ins().fcvt_to_sint_sat(ir::types::I64, x);
        let low = self.builder.ins().ireduce(ir::types::I32, t);
        self.builder.ins().jump(done, &[low.into()]);
        self.builder.switch_to_block(slow);
        let inst = self.emit_call(callee, args)?;
        let called = self.builder.inst_results(inst)[0];
        self.builder.ins().jump(done, &[called.into()]);
        self.builder.switch_to_block(done);
        Ok(r)
    }

    /// The runtime's ToInt32 functions with control flow emitted inline, by symbol (when the
    /// signature is the expected one).
    fn int32_extern(&self, callee: &Callee, params: &[Ty], ret: Ty) -> Option<&'static str> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let symbol = self.program.externs.get(id.0 as usize)?.symbol.as_str();
        match (symbol, params, ret) {
            ("velt_rt_math_to_int32", [Ty::F64], Ty::I32) => Some("velt_rt_math_to_int32"),
            ("velt_rt_math_add_int32", [Ty::I32, Ty::F64], Ty::I32) => {
                Some("velt_rt_math_add_int32")
            }
            _ => None,
        }
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
