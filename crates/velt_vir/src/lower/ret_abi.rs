//! The result and register ABI of functions (the calling convention in lib.rs): where a result
//! goes (an out-pointer for an aggregate or `Result`, a register for a small `struct`, #641),
//! and which parameters are passed as registers (reg_args.rs).

use velt_sema::hir::{DefId, PassMode, TyId, TyKind};

use super::reg_args::{RegParam, MAX_FIELD_PARAMS};

use super::Cx;
use crate::vir::Ty;

/// ABI shape of a function's result.
#[derive(Clone, Copy, Debug)]
pub(super) struct RetAbi {
    /// Type of the trailing out-pointer's pointee (aggregate or `Result` return).
    pub out: Option<Ty>,
    /// VIR return type (`Unit` when there is an out-pointer).
    pub ret: Ty,
    /// A small `struct` value returned in a register (#641): `ret` is `V128`, holding the
    /// struct's bytes (`RegRet`).
    pub reg: Option<RegRet>,
}

/// A `struct` value of at most 16 bytes that is Copy, returned as the `V128` of its bytes
/// (#641): the callee writes the result into a 16-byte slot (`slot`, whose first field is the
/// `V128`) viewed as the struct (`value`) and returns the slot's `V128`; the caller does the
/// reverse. Backends keep both slots in registers, so a `Vec2` comes back in one vector
/// register instead of through memory.
#[derive(Clone, Copy, Debug)]
pub(super) struct RegRet {
    pub value: crate::vir::AggId,
    pub slot: crate::vir::AggId,
}

impl Cx<'_> {
    /// Result ABI of a function returning `ret` and possibly throwing `throws`.
    pub(super) fn ret_abi(&mut self, ret: TyId, throws: Option<TyId>) -> RetAbi {
        let vt = match throws {
            Some(e) => {
                let r = self.intern(TyKind::Result(ret, e));
                self.ty(r)
            }
            None => self.ty(ret),
        };
        if throws.is_none() {
            if let Some(reg) = self.reg_ret(ret, vt) {
                return RetAbi {
                    out: None,
                    ret: Ty::V128,
                    reg: Some(reg),
                };
            }
        }
        match vt {
            Ty::Agg(_) => RetAbi {
                out: Some(vt),
                ret: Ty::Unit,
                reg: None,
            },
            t => RetAbi {
                out: None,
                ret: t,
                reg: None,
            },
        }
    }

    /// The result ABI of an extern: an aggregate always through the out-pointer (C's and the
    /// runtime's layout), never in a register.
    pub(super) fn extern_ret_abi(&mut self, ret: TyId) -> RetAbi {
        match self.ty(ret) {
            vt @ Ty::Agg(_) => RetAbi {
                out: Some(vt),
                ret: Ty::Unit,
                reg: None,
            },
            t => RetAbi {
                out: None,
                ret: t,
                reg: None,
            },
        }
    }

    /// Is a parameter of type `ty` passed with `mode` a `struct` passed in a register: a Copy
    /// one of at most 16 bytes, passed by value (`PassMode::Copy`), as the `V128` of its bytes
    /// (#641)? The callee writes it into a slot of its frame and reads it from there, as from
    /// the pointer it would get otherwise.
    /// A closure's params never are: its code is also the code of its function value, which
    /// callers that do not know it call with every aggregate by pointer. Nor a generated body's
    /// (`generated_instance`): it is called as the generic function it stands in for, whose
    /// param may be a `T` passed by pointer. Nor a method's (`self_ty`): a vtable or an
    /// interface table may call it (`dispatch.rs`), with every aggregate by pointer.
    pub(super) fn reg_param(&mut self, def: DefId, ty: TyId, mode: PassMode) -> Option<RegParam> {
        let f = self.fn_def(def);
        let name = &f.name;
        if mode != PassMode::Copy
            || f.self_ty.is_some()
            || name.contains("{closure")
            || name.starts_with("<generated>")
        {
            return None;
        }
        let vt = self.ty(ty);
        let reg = self.reg_ret(ty, vt)?;
        let mut leaves = vec![];
        match self.scalar_leaves(ty, &mut vec![], &mut leaves) {
            true => Some(RegParam::Fields {
                value: reg.value,
                leaves,
            }),
            false => Some(RegParam::Bytes(reg)),
        }
    }

    /// The scalar fields of the `struct` type `ty`, through nested structs, into `out` with
    /// their paths (`RegParam::Fields`): false when a field is anything else (an option, a
    /// union: their layout's fields need not hold every variant's bytes) or there are more
    /// than `MAX_FIELD_PARAMS`.
    fn scalar_leaves(
        &mut self,
        ty: TyId,
        path: &mut Vec<u32>,
        out: &mut Vec<(Vec<u32>, Ty)>,
    ) -> bool {
        let (Ty::Agg(a), true) = (self.ty(ty), self.value_struct(ty)) else {
            return false;
        };
        let fields: Vec<TyId> = self
            .adt_field_tys(ty)
            .into_iter()
            .filter(|&f| !self.is_unit(f))
            .collect();
        let layout: Vec<Ty> = self.aggs[a.0 as usize].fields.iter().map(|f| f.0).collect();
        if fields.len() != layout.len() {
            return false;
        }
        for (i, (f, lt)) in fields.into_iter().zip(layout).enumerate() {
            path.push(i as u32);
            let ok = match lt {
                Ty::Agg(_) => self.scalar_leaves(f, path, out),
                Ty::Unit | Ty::V128 | Ty::Ptr => false,
                t if self.ty(f) == t => {
                    out.push((path.clone(), t));
                    true
                }
                _ => false,
            };
            path.pop();
            if !ok || out.len() > MAX_FIELD_PARAMS {
                return false;
            }
        }
        true
    }

    /// Is `ret` (VIR type `vt`) a `struct` returned in a register (`RegRet`): Copy, at most 16
    /// bytes, on a target whose functions take `V128` without asking for a feature (not
    /// WebAssembly, whose vectors need `simd128`)?
    fn reg_ret(&mut self, ret: TyId, vt: Ty) -> Option<RegRet> {
        let Ty::Agg(value) = vt else {
            return None;
        };
        if self.target.starts_with("wasm") || !self.value_struct(ret) || self.needs_drop(ret) {
            return None;
        }
        let size = self.aggs[value.0 as usize].size;
        if size > 16 {
            return None;
        }
        // A struct of numbers with padding between them (`{ ok: boolean; v: f64 }`) stays in
        // memory: read as one `V128`, its slot keeps LLVM from splitting it into its fields
        // once the callee is inlined, which costs more than the register saves.
        let mut leaves = vec![];
        if self.scalar_leaves(ret, &mut vec![], &mut leaves)
            && leaves
                .iter()
                .map(|(_, t)| self.size_align(*t).0)
                .sum::<u32>()
                != size
        {
            return None;
        }
        let slot = match self.reg_slot_agg {
            Some(s) => s,
            None => {
                let s = self.new_agg("v128 slot".into(), &[Ty::V128]);
                self.reg_slot_agg = Some(s);
                s
            }
        };
        Some(RegRet { value, slot })
    }
}
