//! Jump-table dispatch for `match`: when every arm selects by the scrutinee's tag (union /
//! enum variants, whose payload patterns only bind) or by integer literals — the shape of
//! `switch` on discriminated unions, enums and numbers — the arms are entered through one VIR
//! `Switch` instead of a chain of tests (an `_` / binding arm, which must be last, is the
//! default target).

use velt_sema::hir::{Lit, Pat, PatKind, TyId, TyKind};

use super::operand::{proj, wrap_int};
use super::FnLower;
use crate::vir::{BlockId, Operand, Place, Proj, Terminator, Ty};

/// The `Switch` operand, the values selecting each arm, and the catch-all arm (if any).
pub(super) struct SwitchPlan {
    pub value: Operand,
    pub keys: Vec<Vec<i128>>,
    pub default: Option<usize>,
}

impl FnLower<'_, '_> {
    /// A jump-table plan for arms `pats` (no guards) on the value at `place`, if they fit.
    pub(super) fn switch_plan(
        &mut self,
        place: &Place,
        ty: TyId,
        pats: &[&Pat],
    ) -> Option<SwitchPlan> {
        let (value, vt) = self.switch_value(place, ty)?;
        let mut keys = vec![];
        let mut default = None;
        for (i, p) in pats.iter().enumerate() {
            match self.pat_keys(p, ty, vt) {
                Some(k) if default.is_none() => keys.push(k),
                None if catch_all(p) && i + 1 == pats.len() => default = Some(i),
                _ => return None,
            }
        }
        (keys.iter().map(Vec::len).sum::<usize>() >= 2).then_some(SwitchPlan {
            value,
            keys,
            default,
        })
    }

    /// What a jump table switches on: the tag of a tagged enum, the discriminant
    /// of a numeric enum, or an integer.
    fn switch_value(&mut self, place: &Place, ty: TyId) -> Option<(Operand, Ty)> {
        let vt = self.cx.ty(ty);
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if matches!(self.cx.hir.def(d), velt_sema::hir::Def::Enum(_)) => {
                if self.cx.is_c_like_enum(d) {
                    Some((Operand::Copy(place.clone()), vt))
                } else {
                    Some((Operand::Copy(proj(place, Proj::Field(0))), Ty::U32))
                }
            }
            TyKind::Int(_) => Some((Operand::Copy(place.clone()), vt)),
            _ => None,
        }
    }

    /// The switch values a pattern selects (`None`: it tests more than the tag / value).
    fn pat_keys(&mut self, p: &Pat, ty: TyId, vt: Ty) -> Option<Vec<i128>> {
        match &p.kind {
            PatKind::Variant { variant, args, .. } if args.iter().all(catch_all) => {
                let super::types::VariantAt::Index(variant) = self.variant_at(p.ty, *variant, ty)
                else {
                    return None;
                };
                match self.cx.kind(ty) {
                    TyKind::Adt(d, _) if self.cx.is_c_like_enum(d) => Some(vec![
                        self.cx.enum_def(d).variants[variant as usize].discriminant as i128,
                    ]),
                    _ => Some(vec![variant as i128]),
                }
            }
            PatKind::Lit(Lit::Int(n)) if matches!(self.cx.kind(ty), TyKind::Int(_)) => {
                Some(vec![wrap_int(*n as i128, vt)])
            }
            PatKind::Or(alts) => {
                let mut out = vec![];
                for a in alts {
                    out.extend(self.pat_keys(a, ty, vt)?);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Enter arm `arms[i]` at `blocks[i]` through one `Switch` (first arm wins on duplicates).
    pub(super) fn emit_switch(&mut self, plan: SwitchPlan, blocks: &[BlockId], fallback: BlockId) {
        let mut cases: Vec<(i128, BlockId)> = vec![];
        for (keys, b) in plan.keys.iter().zip(blocks) {
            for k in keys {
                if !cases.iter().any(|(c, _)| c == k) {
                    cases.push((*k, *b));
                }
            }
        }
        let default = plan.default.map_or(fallback, |i| blocks[i]);
        self.terminate(Terminator::Switch {
            value: plan.value,
            cases,
            default,
        });
    }
}

/// Matches every value without testing it (`_`, a binding).
fn catch_all(p: &Pat) -> bool {
    matches!(p.kind, PatKind::Wildcard | PatKind::Binding(..))
}
