//! Terminators: jumps, branches, switches, returns, calls (direct, extern, indirect) and
//! `unreachable` (VIR only places it after noreturn calls and on paths lowering proved dead).

use rustc_hash::FxHashSet;

use velt_vir::vir::{BlockId, Callee, Operand, Place, Terminator, Ty};

use super::{Emitter, Val};
use crate::rounding;
use crate::runtime;
use crate::string_compare;
use crate::strings;
use crate::types::{abi_ret, abi_type, as_int, global_name, int_bits, int_literal, scalar_type};
use crate::CodegenResult;
use velt_vir::math::MathOp;

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
                unwind,
            } => self.call(callee, args, dest.as_ref(), *next, *unwind)?,
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
        let mut seen = FxHashSet::default();
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
        unwind: Option<BlockId>,
    ) -> CodegenResult<()> {
        if self.resume_call(callee) {
            return Ok(());
        }
        let (params, ret) = self.callee_signature(callee)?;
        let mut values = Vec::with_capacity(args.len());
        let mut raw = Vec::with_capacity(args.len());
        let mut types = Vec::with_capacity(args.len());
        for a in args {
            let (v, ty) = self.scalar(a)?;
            values.push(format!("{} {v}", abi_type(ty)?));
            raw.push(v);
            types.push(ty);
        }
        let variadic = self.variadic(callee);
        let fixed = if variadic {
            params.len().min(types.len())
        } else {
            types.len()
        };
        if params != types[..fixed] {
            bail!("call argument types {types:?} do not match parameters {params:?}");
        }
        let inline = match (self.atomic_call(callee), self.tls_call(callee)) {
            // An atomic instruction in place of the call (atomic.rs).
            (Some(atomic), _) => Some(self.atomic(atomic, &params, ret, &raw)?),
            // A load or store of the thread-local value (tls.rs).
            (None, Some(tls)) => Some(self.tls(tls, &params, ret, &raw)?),
            (None, None) => None,
        };
        if let Some(result) = inline {
            if let (Some(dest), Some(result)) = (dest, result) {
                let loc = self.place(dest)?;
                if loc.ty() != ret && loc.ty() != Ty::Unit {
                    bail!(
                        "inline extern gives {ret:?} but destination has type {:?}",
                        loc.ty()
                    );
                }
                self.write(loc, Val::Scalar(result))?;
            }
            let next = self.block(next)?;
            self.line(format!("br label {next}"));
            return Ok(());
        }
        if self.own_extension_check(callee) {
            // `velt_simd_has_<ext>()` in a function gated on `ext` (#1330), or in code built for
            // a CPU that has `ext` (`--cpu`, whose features the start-up check checks): it runs
            // only where the CPU has it, so the check is true and the emulation behind it is
            // dead code.
            if let Some(dest) = dest {
                let loc = self.place(dest)?;
                self.write(loc, Val::Scalar("1".into()))?;
            }
            let next = self.block(next)?;
            self.line(format!("br label {next}"));
            return Ok(());
        }
        let target = match self.simd_helper(callee, &params, ret) {
            Some(name) => name,
            None => match self.string_fast_path(callee, &params, ret, args) {
                Some(name) => name,
                None => self.call_target(callee)?,
            },
        };
        // A variadic callee's call states its function type: the fixed parameters and `...`.
        let fn_ty = match variadic {
            true => {
                let mut ps: Vec<&str> = params.iter().map(|&t| scalar_type(t)).collect();
                ps.push("...");
                format!(" ({})", ps.join(", "))
            }
            false => String::new(),
        };
        let mut text = format!(
            "call {}{fn_ty} {target}({})",
            abi_ret(ret)?,
            values.join(", ")
        );
        let kept = self.kept_calls.binary_search(&self.block).is_ok();
        if kept || self.enters_gated(callee) {
            text.push_str(" noinline");
        }
        // A call with an unwind edge: an `invoke` landing at the pad's landing block (mod.rs
        // `landing_blocks`). In a function C calls (which has no such edges, mod.rs `unwinds`),
        // an `invoke` whose unwind edge aborts (stop_unwind.rs). Either continues at a label of
        // its own.
        let mut continues = None;
        if let Some(pad) = unwind.filter(|_| self.unwinds) {
            self.block(pad)?;
            self.pads.insert(pad.0);
            let label = format!("bb{}.ok", self.block);
            let rest = text.strip_prefix("call ").unwrap_or(&text);
            text = format!("invoke {rest} to label %{label} unwind label %lp{}", pad.0);
            continues = Some(label);
        } else if self.invokes(&target) {
            let (invoke, label) = self.invoke_text(&text);
            text = invoke;
            continues = Some(label);
        }
        if ret == Ty::Unit {
            self.line(text);
            if let Some(label) = &continues {
                self.label(label);
            }
        } else {
            let result = self.inst(text);
            if let Some(label) = &continues {
                self.label(label);
            }
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

    /// A call of `velt_eh_resume` (vir.rs invariant 14): `resume` of the exception the function's
    /// landing block kept, or (where nothing unwinds) `unreachable`. Whether it was one.
    fn resume_call(&mut self, callee: &Callee) -> bool {
        let Callee::Extern(id) = callee else {
            return false;
        };
        if !(self.program.externs.get(id.0 as usize))
            .is_some_and(|e| velt_vir::eh::is_resume(&e.symbol))
        {
            return false;
        }
        if self.unwinds && super::has_unwind_edges(self.function) {
            let e = self.inst("load ptr, ptr %velt.exn, align 8".into());
            let v = self.inst(format!("insertvalue {{ ptr, i32 }} poison, ptr {e}, 0"));
            let v = self.inst(format!("insertvalue {{ ptr, i32 }} {v}, i32 0, 1"));
            self.line(format!("resume {{ ptr, i32 }} {v}"));
        } else {
            self.line("unreachable".into());
        }
        true
    }

    /// Whether `callee` is a function gated on another CPU target than this one's (#1330). Such a
    /// call stays a call, as in `velt_opt`: where the gated function's features are the
    /// caller's too (`+sha2` on arm64), LLVM would otherwise inline it into ordinary code.
    fn enters_gated(&self, callee: &Callee) -> bool {
        let Callee::Func(id) = callee else {
            return false;
        };
        self.program
            .funcs
            .get(id.0 as usize)
            .is_some_and(|f| f.target.is_some() && f.target != self.function.target)
    }

    /// Whether the CPU the code is built for has `ext` (`--cpu`'s x86 features hold all it
    /// needs).
    fn baseline_has(&self, ext: velt_vir::simd::Ext) -> bool {
        let needs = velt_codegen_cl::ext_x86_features(ext);
        needs != 0 && self.cpu_features & needs == needs
    }

    /// Whether `callee` is `velt_simd_has_<ext>` and this function is gated on `ext`, or the
    /// code is built for a CPU that has `ext`.
    fn own_extension_check(&self, callee: &Callee) -> bool {
        let Callee::Extern(id) = callee else {
            return false;
        };
        let Some(e) = self.program.externs.get(id.0 as usize) else {
            return false;
        };
        matches!(
            velt_vir::simd::SimdCall::of_symbol(&e.symbol),
            Some(velt_vir::simd::SimdCall::Has(ext))
                if (self.function.target.is_some_and(|t| t.covers(ext)) || self.baseline_has(ext))
                    && crate::simd::has_ext(self.simd, ext)
        )
    }

    /// Is `callee` a variadic C function (`vir::ExternFn::variadic`)?
    fn variadic(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Extern(id) if self.program.externs.get(id.0 as usize).is_some_and(|e| e.variadic))
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

    /// The module's inline helper for the SIMD extern `callee` (`simd::helper`, defining it).
    /// (Math operations are statements, `math_target`.)
    fn simd_helper(&mut self, callee: &Callee, params: &[Ty], ret: Ty) -> Option<String> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let e = self.program.externs.get(id.0 as usize)?;
        if let Some((name, definitions, want_params, want_ret)) =
            crate::simd::helper(&e.symbol, self.simd, self.simd_caps)
        {
            if params != want_params || ret != want_ret {
                return None;
            }
            for d in definitions {
                self.intrinsics.need(d);
            }
            return Some(name);
        }
        None
    }

    /// What to call for math operation `op` instead of its runtime function: an LLVM
    /// intrinsic (declaring it) or the module's inline helper (`rounding::helper`,
    /// `runtime::inline_helper`, defining it); `None` for `pow`, which calls the runtime.
    pub(super) fn math_target(&mut self, op: MathOp) -> Option<String> {
        let helper = match op.rounding() {
            Some(how) => rounding::helper(how, self.rounds_by_conversion),
            None => {
                crate::half::helper(op, self.half_native).or_else(|| runtime::inline_helper(op))
            }
        };
        if let Some((name, definitions)) = helper {
            for d in definitions {
                self.intrinsics.need(d.to_string());
            }
            return Some(name.to_string());
        }
        let name = runtime::math_intrinsic(op)?;
        self.intrinsics
            .need(format!("declare double @{name}(double)"));
        Some(format!("@{name}"))
    }

    /// The inline helper (`strings.rs`) to call instead of a runtime string function with a
    /// fast path, defining it in the module.
    fn string_fast_path(
        &mut self,
        callee: &Callee,
        params: &[Ty],
        ret: Ty,
        args: &[Operand],
    ) -> Option<String> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let symbol = &self.program.externs.get(id.0 as usize)?.symbol;
        if let Some((name, def)) =
            string_compare::literal_compare(self.function, symbol, params, ret, args)
        {
            self.intrinsics.need(def);
            return Some(name.to_string());
        }
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
