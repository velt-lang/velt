//! Terminators: jumps, branches, switches, returns, calls (direct, extern, indirect) and
//! `unreachable` (VIR only places it after noreturn calls and on paths lowering proved dead).

use std::collections::HashSet;

use velt_vir::vir::{BlockId, Callee, Operand, Place, Terminator, Ty};

use super::{Emitter, Val};
use crate::rounding;
use crate::runtime;
use crate::strings;
use crate::types::{abi_ret, abi_type, as_int, global_name, int_bits, int_literal, scalar_type};
use crate::CodegenResult;

impl Emitter<'_> {
    pub(super) fn terminator(&mut self, t: &Terminator) -> CodegenResult<()> {
        match t {
            Terminator::Goto(target) => {
                let block = self.block(*target)?;
                self.line(format!("br label {block}"));
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
            Terminator::Unreachable => self.line("unreachable".into()),
        }
        Ok(())
    }

    fn branch(&mut self, cond: &Operand, then: BlockId, els: BlockId) -> CodegenResult<()> {
        let (c, ty) = self.scalar(cond)?;
        if ty != Ty::Bool {
            bail!("branch condition must be Bool, found {ty:?}");
        }
        let (then, els) = (self.block(then)?, self.block(els)?);
        let bit = self.inst(format!("icmp ne i8 {c}, 0"));
        self.line(format!("br i1 {bit}, label {then}, label {els}"));
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
        let v = if ty == Ty::Ptr {
            self.inst(format!("ptrtoint ptr {v} to i64"))
        } else {
            v
        };
        let t = format!("i{}", int_bits(int_ty));
        // Case values are compared at the value's width: (possibly negative) keys are
        // reduced to their bit pattern, and must stay distinct after that.
        let mut seen = HashSet::new();
        let mut arms = String::new();
        for (k, target) in cases {
            let key = int_literal(int_ty, *k);
            if !seen.insert(key.clone()) {
                bail!("duplicate switch case {k}");
            }
            arms.push_str(&format!(" {t} {key}, label {}", self.block(*target)?));
        }
        let default = self.block(default)?;
        self.line(format!("switch {t} {v}, label {default} [{arms} ]"));
        Ok(())
    }

    fn ret(&mut self, op: &Operand) -> CodegenResult<()> {
        let ret = self.function.ret;
        if ret == Ty::Unit {
            self.line("ret void".into());
            return Ok(());
        }
        let (v, ty) = self.scalar(op)?;
        if ty != ret {
            bail!("returning {ty:?} from a function returning {ret:?}");
        }
        self.line(format!("ret {} {v}", scalar_type(ret)));
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
            values.push(format!("{} {v}", abi_type(ty)?));
            types.push(ty);
        }
        if params != types {
            bail!("call argument types {types:?} do not match parameters {params:?}");
        }
        let target = match self.math_intrinsic(callee, &params, ret) {
            Some(name) => name,
            None => match self.string_fast_path(callee, &params, ret) {
                Some(name) => name,
                None => self.call_target(callee)?,
            },
        };
        let text = format!("call {} {target}({})", abi_ret(ret)?, values.join(", "));
        if ret == Ty::Unit {
            self.line(text);
        } else {
            let result = self.inst(text);
            if let Some(dest) = dest {
                let loc = self.place(dest)?;
                if loc.ty() != ret && loc.ty() != Ty::Unit {
                    bail!(
                        "call returns {ret:?} but destination has type {:?}",
                        loc.ty()
                    );
                }
                self.write(loc, Val::Scalar(result))?;
            }
        }
        let next = self.block(next)?;
        self.line(format!("br label {next}"));
        Ok(())
    }

    fn callee_signature(&self, callee: &Callee) -> CodegenResult<(Vec<Ty>, Ty)> {
        Ok(match callee {
            Callee::Func(id) => match self.program.funcs.get(id.0 as usize) {
                Some(f) => (f.params.clone(), f.ret),
                None => bail!("unknown function #{}", id.0),
            },
            Callee::Extern(id) => match self.program.externs.get(id.0 as usize) {
                Some(e) => (e.params.clone(), e.ret),
                None => bail!("unknown extern #{}", id.0),
            },
            Callee::Ptr { params, ret, .. } => (params.clone(), *ret),
        })
    }

    /// `@llvm.<op>.f64` when `callee` is a runtime math function with an exact intrinsic
    /// equivalent (declaring the intrinsic), or the module's inline helper for it
    /// (`rounding::helper`, `runtime::inline_helper`, defining the helper).
    fn math_intrinsic(&mut self, callee: &Callee, params: &[Ty], ret: Ty) -> Option<String> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let symbol = &self.program.externs.get(id.0 as usize)?.symbol;
        if let Some((name, definitions)) = rounding::helper(symbol, self.rounds_by_conversion) {
            if params != [Ty::F64] || ret != Ty::F64 {
                return None;
            }
            for d in definitions {
                self.intrinsics.need(d.to_string());
            }
            return Some(name.to_string());
        }
        if let Some((name, definitions, want_params, want_ret)) = runtime::inline_helper(symbol) {
            if params != want_params || ret != want_ret {
                return None;
            }
            for d in definitions {
                self.intrinsics.need(d.to_string());
            }
            return Some(name.to_string());
        }
        let name = runtime::math_intrinsic(symbol)?;
        if params != [Ty::F64] || ret != Ty::F64 {
            return None;
        }
        self.intrinsics
            .need(format!("declare double @{name}(double)"));
        Some(format!("@{name}"))
    }

    /// The inline helper (`strings.rs`) to call instead of a runtime string function with a
    /// fast path, defining it in the module.
    fn string_fast_path(&mut self, callee: &Callee, params: &[Ty], ret: Ty) -> Option<String> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let symbol = &self.program.externs.get(id.0 as usize)?.symbol;
        let (name, defs) = strings::fast_path(symbol, params, ret, self.wide_pointer_slots)?;
        for d in defs {
            self.intrinsics.need(d);
        }
        Some(name.to_string())
    }

    /// The called value: a symbol, or the pointer operand of an indirect call (with opaque
    /// pointers, the call site's own type is the signature).
    fn call_target(&mut self, callee: &Callee) -> CodegenResult<String> {
        Ok(match callee {
            Callee::Func(id) => global_name(&self.program.func(*id).symbol),
            Callee::Extern(id) => global_name(&self.program.ext(*id).symbol),
            Callee::Ptr { target, .. } => {
                let (target, ty) = self.scalar(target)?;
                if ty != Ty::Ptr {
                    bail!("indirect call target must be Ptr, found {ty:?}");
                }
                target
            }
        })
    }
}
