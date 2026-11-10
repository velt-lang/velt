//! Calls of variadic C functions (`vir::ExternFn::variadic`: `fcntl`, `printf`). Cranelift has
//! no variadic calls, so each is an ordinary call whose own signature puts every argument where
//! the target's variadic convention does (rustc_codegen_cranelift does the same):
//!
//! - **AAPCS64** (Linux arm64): variadic arguments go where named ones would.
//! - **Apple arm64**: every variadic argument is on the stack in an 8-byte slot. Zeros fill the
//!   8 integer registers (and the 8 float ones when a `double` follows), and each variadic
//!   argument is passed as 64 bits, so the ordinary convention puts them on the stack in order.
//! - **SysV x86-64** (Linux, macOS): variadic arguments go where named ones would, and `%al`
//!   bounds the vector registers used, which Cranelift can't set. A callee only reads `%al` to
//!   decide whether to save them, so integers and pointers are passed; a `double` is refused,
//!   naming the declaration (an LLVM build, `--release`, passes it).
//! - **Windows** x64 and arm64: a variadic `double` goes in an integer register (x64 also in the
//!   float one, which a callee reading `va_arg` doesn't look at), so its bits pass as an
//!   integer. On arm64 every float argument of a variadic call goes in integer registers.
//!
//! The arguments arrive promoted (VIR: `I32`, `U32`, `I64`, `U64`, `F64`, `Ptr`).

use cranelift_codegen::ir::{self, InstBuilder};
use target_lexicon::{Architecture, OperatingSystem};
use velt_vir::vir::{ExternId, Ty};

use super::Translator;
use crate::abi::make_signature;
use crate::CodegenResult;

/// Registers of each class an arm64 call passes arguments in.
const ARM64_REGS: usize = 8;

/// The target's variadic convention, as far as a call differs from an ordinary one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Convention {
    /// Variadic arguments go where named ones would (AAPCS64, SysV x86-64).
    Plain { sysv_x86: bool },
    /// Apple arm64: on the stack, 8 bytes each.
    AppleArm64,
    /// Windows: variadic floats in integer registers (`all_floats`: fixed ones too, arm64).
    Windows { all_floats: bool },
}

impl Convention {
    fn of(triple: &target_lexicon::Triple) -> Convention {
        let arm = matches!(triple.architecture, Architecture::Aarch64(_));
        match triple.operating_system {
            OperatingSystem::Windows => Convention::Windows { all_floats: arm },
            OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_) if arm => {
                Convention::AppleArm64
            }
            _ => Convention::Plain {
                sysv_x86: triple.architecture == Architecture::X86_64,
            },
        }
    }
}

impl Translator<'_, '_> {
    /// A call of variadic C function `id` with `values` (of `types`), the first `fixed` of them
    /// its named parameters.
    pub(super) fn variadic_call(
        &mut self,
        id: ExternId,
        values: &[ir::Value],
        types: &[Ty],
        fixed: usize,
    ) -> CodegenResult<ir::Inst> {
        let Some(e) = self.program.externs.get(id.0 as usize) else {
            bail!("unknown extern #{}", id.0)
        };
        let (ret, symbol) = (e.ret, e.symbol.clone());
        let decl = e.c_decl.clone();
        let convention = Convention::of(self.module.isa().triple());
        let mut vals: Vec<ir::Value> = vec![];
        let mut tys: Vec<Ty> = vec![];
        let apple = convention == Convention::AppleArm64;
        let variadic_floats = types[fixed..].iter().any(|t| t.is_float());
        if apple && fixed == 0 {
            self.apple_padding(&mut vals, &mut tys, variadic_floats);
        }
        for (k, (&v, &t)) in values.iter().zip(types).enumerate() {
            let float = t.is_float();
            match convention {
                Convention::Windows { all_floats } if float && (all_floats || k >= fixed) => {
                    vals.push(self.float_bits(v, t));
                    tys.push(Ty::I64);
                }
                Convention::Plain { sysv_x86: true } if float && k >= fixed => {
                    let at = decl.as_deref().unwrap_or(&symbol);
                    bail!(
                        "C function `{symbol}` (declared at {at}) is passed a float variadic argument, which this code generator (Cranelift, debug builds) can't pass on x86-64 Linux and macOS: build with `--release` (LLVM)"
                    );
                }
                _ => {
                    vals.push(v);
                    tys.push(t);
                }
            }
            if apple && k + 1 == fixed {
                self.apple_padding(&mut vals, &mut tys, variadic_floats);
            }
        }
        if apple {
            // Each variadic argument fills its 8-byte slot.
            let start = tys.len() - (types.len() - fixed);
            for k in start..tys.len() {
                let wide = match tys[k] {
                    Ty::I32 => Some(self.builder.ins().sextend(ir::types::I64, vals[k])),
                    Ty::U32 => Some(self.builder.ins().uextend(ir::types::I64, vals[k])),
                    _ => None,
                };
                if let Some(w) = wide {
                    vals[k] = w;
                    tys[k] = Ty::I64;
                }
            }
        }
        let func = self.extern_id(id)?;
        let r = self.module.declare_func_in_func(func, self.builder.func);
        let sig = make_signature(self.decls.call_conv, &tys, ret)?;
        self.builder.func.dfg.ext_funcs[r].signature = self.builder.import_signature(sig);
        Ok(self.builder.ins().call(r, &vals))
    }

    /// Zeros after the named arguments `tys` (Apple arm64): up to the 8 integer registers, and
    /// the 8 float registers too when a variadic `double` follows, so the variadic arguments
    /// after them go on the stack.
    fn apple_padding(&mut self, vals: &mut Vec<ir::Value>, tys: &mut Vec<Ty>, floats: bool) {
        let ints = tys.iter().filter(|t| !t.is_float()).count();
        for _ in ints..ARM64_REGS {
            vals.push(self.builder.ins().iconst(ir::types::I64, 0));
            tys.push(Ty::I64);
        }
        if floats {
            let n = tys.iter().filter(|t| t.is_float()).count();
            for _ in n..ARM64_REGS {
                vals.push(self.builder.ins().f64const(0.0));
                tys.push(Ty::F64);
            }
        }
    }

    /// The bits of float `v` (of type `t`) as an `I64`.
    fn float_bits(&mut self, v: ir::Value, t: Ty) -> ir::Value {
        let flags = ir::MemFlags::new();
        match t {
            Ty::F32 => {
                let bits = self.builder.ins().bitcast(ir::types::I32, flags, v);
                self.builder.ins().uextend(ir::types::I64, bits)
            }
            _ => self.builder.ins().bitcast(ir::types::I64, flags, v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_target_has_its_variadic_convention() {
        let of = |t: &str| Convention::of(&t.parse().unwrap());
        assert_eq!(of("aarch64-apple-darwin"), Convention::AppleArm64);
        assert_eq!(
            of("x86_64-apple-darwin"),
            Convention::Plain { sysv_x86: true }
        );
        assert_eq!(
            of("x86_64-unknown-linux-gnu"),
            Convention::Plain { sysv_x86: true }
        );
        assert_eq!(
            of("aarch64-unknown-linux-gnu"),
            Convention::Plain { sysv_x86: false }
        );
        assert_eq!(
            of("x86_64-pc-windows-msvc"),
            Convention::Windows { all_floats: false }
        );
        assert_eq!(
            of("aarch64-pc-windows-msvc"),
            Convention::Windows { all_floats: true }
        );
    }
}
