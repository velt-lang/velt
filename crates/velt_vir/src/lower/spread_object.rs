//! `Intrinsic::SpreadObject` (#350; hir_encodings.md "Spreads in generic bodies"): an object
//! literal that spreads a value whose fields are known only per instantiation (`{ ...t, $set }`
//! with `t: T`). Its type is the deferred `TyOp::Spread`, which sema reduced at this
//! instantiation to the object type JavaScript's spread builds. Each part is a place of a
//! concrete object type (a spread source, or a temporary holding a run of explicit properties);
//! each field of the result is shared from the last part that has a field of that name, and an
//! optional field (which sema allows only from one part) keeps that part's presence.

use velt_sema::hir::{self, TyId, TyKind};

use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place};

impl FnLower<'_, '_> {
    pub(super) fn spread_object(&mut self, parts: &[hir::Expr], ty: TyId) -> Operand {
        let rty = self.sub(ty);
        let srcs: Vec<(Place, TyId)> = parts
            .iter()
            .map(|p| (self.place_expr(p), self.sub(p.ty)))
            .collect();
        let TyKind::Adt(d, _) = self.cx.kind(rty) else {
            ice(format_args!(
                "a spread object of the non-object type {rty:?}"
            ))
        };
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tys = self.cx.adt_field_tys(rty);
        let mut ops = vec![];
        let mut flags = vec![];
        for (i, name) in names.iter().enumerate() {
            let Some((place, sty, k)) = srcs.iter().rev().find_map(|(p, sty)| {
                let TyKind::Adt(sd, _) = self.cx.kind(*sty) else {
                    return None;
                };
                let k = self
                    .cx
                    .adt_def(sd)
                    .fields
                    .iter()
                    .position(|f| f.name == *name)?;
                Some((p.clone(), *sty, k as u32))
            }) else {
                ice(format_args!("spread field `{name}` has no source"))
            };
            let fty = tys[i];
            let from = self.cx.member_ty(sty, k);
            if self.vty(from) != self.vty(fty) {
                ice(format_args!(
                    "spread field `{name}` of another layout than its source"
                ))
            }
            if self.cx.presence_slot(rty, i as u32).is_some() {
                let present = match self.cx.presence_slot(sty, k) {
                    Some(slot) => {
                        let fp = self.field_place(&place, sty, k);
                        let mut flag = fp.clone();
                        if let Some(crate::vir::Proj::Field(f)) = flag.proj.last_mut() {
                            *f = slot;
                        }
                        Operand::Copy(flag)
                    }
                    None => FnLower::ctrue(),
                };
                flags.push(present);
            }
            if self.cx.is_unit(fty) {
                ops.push(unit());
                continue;
            }
            let fp = self.field_place(&place, sty, k);
            ops.push(self.share_value(Operand::Copy(fp), fty));
        }
        ops.extend(flags);
        self.build_agg(rty, ops)
    }
}
