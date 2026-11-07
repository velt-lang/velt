//! Small `struct` values passed and returned in registers (abi.rs `reg_param`, `reg_ret`): as
//! their scalar fields or as the `V128` of their bytes, on both sides of a call.

use velt_sema::hir::{self, UseMode};
use velt_sema::hir::{DefId, PassMode, TyId};

use super::abi::RegRet;
use super::FnLower;
use crate::vir::{self, Local, Operand, Place, Proj, Rvalue, Ty};

/// How a small `struct` param is passed in registers (#641, `Cx::reg_param`).
#[derive(Clone, Debug)]
pub(super) enum RegParam {
    /// As its scalar fields (through nested structs), each its own param: the field path from
    /// the struct to each, and its type. A `Vec2` arrives in two float registers, as Rust and C
    /// pass a struct of two floats, with no lanes to pack and unpack.
    Fields {
        value: crate::vir::AggId,
        leaves: Vec<(Vec<u32>, Ty)>,
    },
    /// As the `V128` of its bytes (`RegRet`): a struct with more scalar fields than
    /// `MAX_FIELD_PARAMS`, or with a field that is not a number or a boolean.
    Bytes(RegRet),
}

/// The most scalar fields a `struct` param is passed as (`RegParam::Fields`); one with more
/// travels as its bytes. Four covers every struct of at most 16 bytes made of 4-byte fields.
pub(super) const MAX_FIELD_PARAMS: usize = 4;

impl RegParam {
    /// The VIR params it is passed as.
    pub(super) fn tys(&self) -> Vec<Ty> {
        match self {
            RegParam::Fields { leaves, .. } => leaves.iter().map(|(_, t)| *t).collect(),
            RegParam::Bytes(_) => vec![Ty::V128],
        }
    }
}

impl FnLower<'_, '_> {
    /// How an argument of type `ty` (of this instance, before substitution) to `callee`'s param
    /// passed with `mode` is passed in registers (abi.rs `reg_param`), if it is.
    pub(super) fn arg_reg(
        &mut self,
        callee: Option<DefId>,
        ty: TyId,
        mode: PassMode,
    ) -> Option<RegParam> {
        let d = callee?;
        let t = self.sub(ty);
        self.cx.reg_param(d, t, mode)
    }

    /// The operands of the `struct` value `v` passed in registers (abi.rs `reg_param`): its
    /// scalar fields, read now, or the `V128` of its bytes, through a 16-byte slot it is
    /// written to.
    pub(super) fn reg_arg(&mut self, v: Operand, reg: &RegParam) -> Vec<Operand> {
        let reg = match reg {
            RegParam::Fields { value, leaves } => {
                let place = match v {
                    Operand::Copy(p) => p,
                    c => Place::local(self.copy_to_temp(c, Ty::Agg(*value))),
                };
                return leaves
                    .iter()
                    .map(|(path, t)| {
                        let mut f = place.clone();
                        f.proj.extend(path.iter().map(|&i| Proj::Field(i)));
                        self.rvalue_temp(*t, Rvalue::Use(Operand::Copy(f)))
                    })
                    .collect();
            }
            RegParam::Bytes(r) => *r,
        };
        let slot = self.temp(Ty::Agg(reg.slot));
        let a = self.addr(Place::local(slot));
        let p = self.temp(Ty::Ptr);
        self.assign(Place::local(p), Rvalue::Use(a));
        let as_value = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::Agg(reg.value))],
        };
        self.assign(as_value, Rvalue::Use(v));
        let as_v128 = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::V128)],
        };
        vec![self.rvalue_temp(Ty::V128, Rvalue::Use(Operand::Copy(as_v128)))]
    }

    /// The `struct` value a call returned in the register `d` (abi.rs `RegRet`): a local of the
    /// struct's type.
    pub(super) fn reg_value(&mut self, d: vir::Local, reg: RegRet) -> vir::Local {
        // Through a pointer: a place read as another layout than it was written as.
        let slot = self.temp(Ty::Agg(reg.slot));
        let a = self.addr(Place::local(slot));
        let p = self.temp(Ty::Ptr);
        self.assign(Place::local(p), Rvalue::Use(a));
        let v = Operand::Copy(Place::local(d));
        let as_v128 = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::V128)],
        };
        self.assign(as_v128, Rvalue::Use(v));
        let t = self.temp(Ty::Agg(reg.value));
        let viewed = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::Agg(reg.value))],
        };
        self.assign(Place::local(t), Rvalue::Use(Operand::Copy(viewed)));
        t
    }
}

impl FnLower<'_, '_> {
    /// A param passed in registers (`Cx::reg_param`), the locals `vs` of its VIR params:
    /// written to a slot of the frame, and the pointer to that slot, read as a param passed by
    /// pointer is.
    pub(super) fn reg_incoming(&mut self, vs: &[Local], reg: &RegParam) -> Local {
        let slot_ty = match reg {
            RegParam::Fields { value, .. } => Ty::Agg(*value),
            RegParam::Bytes(r) => Ty::Agg(r.slot),
        };
        let slot = self.new_local(slot_ty, None);
        let p = self.new_local(Ty::Ptr, None);
        let a = self.addr(Place::local(slot));
        self.assign(Place::local(p), Rvalue::Use(a));
        match reg {
            RegParam::Fields { value, leaves } => {
                for ((path, _), v) in leaves.iter().zip(vs) {
                    let mut proj = vec![Proj::Deref(Ty::Agg(*value))];
                    proj.extend(path.iter().map(|&i| Proj::Field(i)));
                    let field = Place { local: p, proj };
                    self.assign(field, Rvalue::Use(Operand::Copy(Place::local(*v))));
                }
            }
            RegParam::Bytes(_) => {
                let as_v128 = Place {
                    local: p,
                    proj: vec![Proj::Deref(Ty::V128)],
                };
                self.assign(as_v128, Rvalue::Use(Operand::Copy(Place::local(vs[0]))));
            }
        }
        p
    }
}

/// A Copy value read out of a place (passed by pointer, it would still point into the place).
pub(super) fn is_copy_read(a: &hir::Expr) -> bool {
    if let hir::ExprKind::Downcast(x) = &a.kind {
        return is_copy_read(x);
    }
    matches!(
        a.kind,
        hir::ExprKind::Local(_, UseMode::Copy)
            | hir::ExprKind::Field {
                mode: UseMode::Copy,
                ..
            }
            | hir::ExprKind::Index {
                mode: UseMode::Copy,
                ..
            }
            | hir::ExprKind::UnwrapSome(_, UseMode::Copy)
            | hir::ExprKind::UnwrapVariant {
                mode: UseMode::Copy,
                ..
            }
    )
}
