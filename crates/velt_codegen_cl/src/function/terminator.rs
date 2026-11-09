//! Terminators: jumps, branches, switches, returns, calls (direct, extern, indirect), traps.

use rustc_hash::FxHashSet;

use cranelift_codegen::ir::{
    self, BlockArg, BlockCall, ExceptionTableData, ExceptionTableItem, InstBuilder, TrapCode,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{Switch, Variable};
use cranelift_module::FuncId;
use velt_vir::math::MathOp;
use velt_vir::vir::{BlockId, Callee, Operand, Place, Terminator, Ty};

use super::simd::Inlined;
use super::{Translator, Val};
use crate::abi::{as_int, int_bits, make_signature, scalar_type};
use crate::CodegenResult;

impl Translator<'_, '_> {
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
                unwind,
            } => self.call(callee, args, dest.as_ref(), *next, *unwind)?,
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
        let mut seen = FxHashSet::default();
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
        unwind: Option<BlockId>,
    ) -> CodegenResult<()> {
        if self.resume(callee)? {
            return Ok(());
        }
        let (params, ret) = self.callee_signature(callee)?;
        let mut values = Vec::with_capacity(args.len());
        let mut types = Vec::with_capacity(args.len());
        for a in args {
            let (v, ty) = self.scalar(a)?;
            values.push(v);
            types.push(ty);
        }
        if let Callee::Extern(id) = callee {
            let variadic = (self.program.externs.get(id.0 as usize)).is_some_and(|e| e.variadic);
            if variadic && types.len() >= params.len() && params == types[..params.len()] {
                let inst = self.variadic_call(*id, &values, &types, params.len())?;
                let result = self.builder.inst_results(inst).first().copied();
                return self.call_result(dest, ret, result, next);
            }
        }
        if params != types {
            bail!("call argument types {types:?} do not match parameters {params:?}");
        }
        let result = match self
            .inline_simd(callee, args, &values, &params, ret)
            .or_else(|| self.inline_atomic(callee, &values, &params, ret))
            .or_else(|| self.inline_tls(callee, &values))
        {
            Some(Inlined::Value(v)) => Some(v),
            Some(Inlined::Unit) => None,
            None => match self.try_callee(callee, unwind, &params)? {
                Some((target, pad)) => self.try_call(target, &values, ret, pad)?,
                None => {
                    let inst = self.emit_call(callee, &values)?;
                    self.builder.inst_results(inst).first().copied()
                }
            },
        };
        self.call_result(dest, ret, result, next)
    }

    /// Store call `result` (of type `ret`) at `dest` and continue at `next`.
    fn call_result(
        &mut self,
        dest: Option<&Place>,
        ret: Ty,
        result: Option<ir::Value>,
        next: BlockId,
    ) -> CodegenResult<()> {
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

    /// A call of `velt_eh_resume` (vir.rs invariant 14): `_Unwind_Resume` of the exception the
    /// landing block kept, or (where nothing unwinds) a trap. Whether it was one.
    fn resume(&mut self, callee: &Callee) -> CodegenResult<bool> {
        let Callee::Extern(id) = callee else {
            return Ok(false);
        };
        if !(self.program.externs.get(id.0 as usize))
            .is_some_and(|e| velt_vir::eh::is_resume(&e.symbol))
        {
            return Ok(false);
        }
        match self.decls.unwind_resume.filter(|_| self.unwinds()) {
            Some(f) => {
                let exn = self.exn_var();
                let e = self.builder.use_var(exn);
                let r = self.func_ref(f);
                self.builder.ins().call(r, &[e]);
                self.builder.ins().trap(TrapCode::unwrap_user(1));
            }
            None => {
                self.builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
        Ok(true)
    }

    /// Whether this function's calls with an unwind edge are `try_call`s: where calls unwind,
    /// except in a function C calls, which has no unwind entry there (`Function::stops_unwind`):
    /// an unwind stops before it and its cleanup pads could never run.
    fn unwinds(&self) -> bool {
        self.decls.unwinds && !self.function.stops_unwind
    }

    fn exn_var(&mut self) -> Variable {
        match self.exn {
            Some(v) => v,
            None => {
                let v = self.builder.declare_var(ir::types::I64);
                self.exn = Some(v);
                v
            }
        }
    }

    /// What a call with the unwind edge `unwind` calls through a `try_call`, and its pad: where
    /// calls unwind, a function or function value whose arguments are all in registers when the
    /// module's convention isn't SystemV (`try_call` exists only for SystemV, which agrees with
    /// Apple's arm64 convention for register arguments; their extension attributes apply in
    /// both). `None`: a plain call.
    fn try_callee(
        &mut self,
        callee: &Callee,
        unwind: Option<BlockId>,
        params: &[Ty],
    ) -> CodegenResult<Option<(TryTarget, BlockId)>> {
        let Some(pad) = unwind.filter(|_| self.unwinds()) else {
            return Ok(None);
        };
        let system_v = self.decls.call_conv == CallConv::SystemV;
        if !system_v {
            let floats = params
                .iter()
                .filter(|t| t.is_float() || **t == Ty::V128)
                .count();
            if floats > 8 || params.len() - floats > 8 {
                return Ok(None);
            }
        }
        let target = match callee {
            Callee::Func(id) => {
                let func = self.func_id(*id)?;
                let r = self.func_ref(func);
                if system_v {
                    TryTarget::Direct(r)
                } else {
                    TryTarget::Direct(self.system_v_ref(func, r))
                }
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
                let sig = make_signature(CallConv::SystemV, params, *ret)?;
                TryTarget::Indirect(target, self.builder.import_signature(sig))
            }
            Callee::Extern(_) => return Ok(None),
        };
        Ok(Some((target, pad)))
    }

    /// A reference to `func` (referenced as `r`) with its signature in the SystemV convention.
    fn system_v_ref(&mut self, func: FuncId, r: ir::FuncRef) -> ir::FuncRef {
        if let Some(r) = self.try_refs.get(&func) {
            return *r;
        }
        let mut data = self.builder.func.dfg.ext_funcs[r].clone();
        let mut sig = self.builder.func.dfg.signatures[data.signature].clone();
        sig.call_conv = CallConv::SystemV;
        data.signature = self.builder.import_signature(sig);
        let sv = self.builder.import_function(data);
        self.try_refs.insert(func, sv);
        sv
    }

    /// `target(args)` as a `try_call` whose exceptional edge goes to `pad`'s landing block (which
    /// keeps the exception for `velt_eh_resume`); continues in a new block. Its result, if any.
    fn try_call(
        &mut self,
        target: TryTarget,
        args: &[ir::Value],
        ret: Ty,
        pad: BlockId,
    ) -> CodegenResult<Option<ir::Value>> {
        let ok = self.builder.create_block();
        let result = match ret {
            Ty::Unit => None,
            t => Some(self.builder.append_block_param(ok, scalar_type(t))),
        };
        let (landing, new) = match self.landings.get(&pad.0) {
            Some(&b) => (b, false),
            None => {
                let b = self.builder.create_block();
                self.builder.append_block_param(b, ir::types::I64);
                self.landings.insert(pad.0, b);
                (b, true)
            }
        };
        let sig = match target {
            TryTarget::Direct(r) => self.builder.func.dfg.ext_funcs[r].signature,
            TryTarget::Indirect(_, sig) => sig,
        };
        let pool = &mut self.builder.func.dfg.value_lists;
        let rets: Vec<BlockArg> = result.iter().map(|_| BlockArg::TryCallRet(0)).collect();
        let normal = BlockCall::new(ok, rets, pool);
        let caught = BlockCall::new(landing, [BlockArg::TryCallExn(0)], pool);
        let table = ExceptionTableData::new(sig, normal, [ExceptionTableItem::Default(caught)]);
        let table = self.builder.func.dfg.exception_tables.push(table);
        match target {
            TryTarget::Direct(r) => self.builder.ins().try_call(r, args, table),
            TryTarget::Indirect(t, _) => self.builder.ins().try_call_indirect(t, args, table),
        };
        if new {
            let pad_block = self.block(pad)?;
            self.builder.switch_to_block(landing);
            let e = self.builder.block_params(landing)[0];
            let exn = self.exn_var();
            self.builder.def_var(exn, e);
            self.builder.ins().jump(pad_block, &[]);
        }
        self.builder.switch_to_block(ok);
        Ok(result)
    }

    fn callee_signature(&self, callee: &Callee) -> CodegenResult<(Vec<Ty>, Ty)> {
        Ok(match callee {
            Callee::Func(id) => {
                self.func_id(*id)?;
                let f = self.program.func(*id);
                (f.params.clone(), f.ret)
            }
            Callee::Extern(id) => {
                let Some(e) = self.program.externs.get(id.0 as usize) else {
                    bail!("unknown extern #{}", id.0)
                };
                (e.params.clone(), e.ret)
            }
            Callee::Ptr { params, ret, .. } => (params.clone(), *ret),
        })
    }

    /// The math operation `op` on `values`: inline where Cranelift has an instruction or a
    /// sequence for it, otherwise (`pow`) a call of its runtime function `callee`, which the
    /// inline ToInt32 also calls on its cold path.
    pub(super) fn math_value(
        &mut self,
        op: MathOp,
        callee: &Callee,
        values: &[ir::Value],
    ) -> CodegenResult<ir::Value> {
        match op {
            // ToInt32(x); the slow path calls the runtime with the same arguments.
            MathOp::ToInt32 => self.inline_to_int32(callee, values[0], values),
            // ToInt32(a + x), always through the double sum (`velt_rt_math_add_int32`).
            MathOp::AddInt32 => {
                let af = self.builder.ins().fcvt_from_sint(ir::types::F64, values[0]);
                let sum = self.builder.ins().fadd(af, values[1]);
                self.inline_to_int32(callee, sum, values)
            }
            _ => match self.inline_math(op, values) {
                Some(v) => Ok(v),
                None => {
                    let inst = self.emit_call(callee, values)?;
                    match self.builder.inst_results(inst).first() {
                        Some(&v) => Ok(v),
                        None => bail!("math operation {op:?} gave no value"),
                    }
                }
            },
        }
    }

    /// The runtime math operations that are exactly one Cranelift instruction
    /// (rt_abi_async.md §9, `Math.clz32`, `Math.umulh`, JS's int32 multiply) or a short sequence
    /// (the rounding functions, `rounding.rs`) are emitted inline instead of called; `None` for
    /// the others (`pow`), which stay calls.
    fn inline_math(&mut self, op: MathOp, args: &[ir::Value]) -> Option<ir::Value> {
        if let Some(how) = op.rounding() {
            return Some(self.round_f64(args[0], how));
        }
        if let Some(v) = self.half_conversion(op, args[0]) {
            return Some(v);
        }
        let flags = ir::MemFlags::new();
        Some(match op {
            MathOp::Sqrt | MathOp::SqrtF32 => self.builder.ins().sqrt(args[0]),
            MathOp::Abs => self.builder.ins().fabs(args[0]),
            MathOp::Clz32 | MathOp::Clz64 => self.builder.ins().clz(args[0]),
            MathOp::Ctz64 => self.builder.ins().ctz(args[0]),
            MathOp::Popcount64 => self.builder.ins().popcnt(args[0]),
            MathOp::F64Bits => self.builder.ins().bitcast(ir::types::I64, flags, args[0]),
            MathOp::BitsF64 => self.builder.ins().bitcast(ir::types::F64, flags, args[0]),
            MathOp::F32Bits => self.builder.ins().bitcast(ir::types::I32, flags, args[0]),
            MathOp::BitsF32 => self.builder.ins().bitcast(ir::types::F32, flags, args[0]),
            MathOp::Umulh => self.builder.ins().umulhi(args[0], args[1]),
            MathOp::MulInt32 => {
                // The exact product, rounded through a double like JS's multiply (|p| < 2^62).
                use ir::types::{F64, I32, I64};
                let x = self.builder.ins().sextend(I64, args[0]);
                let y = self.builder.ins().sextend(I64, args[1]);
                let p = self.builder.ins().imul(x, y);
                let d = self.builder.ins().fcvt_from_sint(F64, p);
                let t = self.builder.ins().fcvt_to_sint_sat(I64, d);
                self.builder.ins().ireduce(I32, t)
            }
            MathOp::F16ToF32
            | MathOp::F32ToF16
            | MathOp::F64ToF16
            | MathOp::Bf16ToF32
            | MathOp::F32ToBf16
            | MathOp::F64ToBf16
            | MathOp::I64ToBf16
            | MathOp::U64ToBf16
            | MathOp::F16Bits
            | MathOp::BitsF16
            | MathOp::Bf16Bits
            | MathOp::BitsBf16 => unreachable!("ICE: half conversions are handled above"),
            MathOp::ToInt32
            | MathOp::AddInt32
            | MathOp::PowF64
            | MathOp::PowI64
            | MathOp::Floor
            | MathOp::Ceil
            | MathOp::Trunc
            | MathOp::Round => return None,
        })
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

/// What a `try_call` calls: a function (by a SystemV reference) or a function value.
#[derive(Clone, Copy)]
enum TryTarget {
    Direct(ir::FuncRef),
    Indirect(ir::Value, ir::SigRef),
}
