//! The setter slots of an object literal implementing an interface (`iface_object`): how the
//! literal stores each field of the interface, for `crate::iface_writes`.

use velt_common::Span;

use super::iface_object::Slot;
use crate::hir::{DefId, TyId, TyKind};
use crate::iface_writes::SetterSrc;

/// How the literal stores each field of `iface` (`n_methods` methods), for the setter
/// slots of `crate::iface_writes`: a plain value in its field of struct `d`, a `get f()` through
/// the `set f(v)` stored next to it, or missing.
pub(super) fn literal_setters(
    cx: &crate::ctx::Ctx,
    (iface, n_methods): (DefId, usize),
    slots: &[Option<Slot>],
    field_tys: &[(String, TyId)],
    (d, self_ty): (DefId, TyId),
    this_of: &dyn Fn(u32) -> Option<(TyId, u32)>,
    span: Span,
) -> Vec<SetterSrc> {
    let names: Vec<(String, bool)> = cx.iface(iface).map_or(vec![], |i| {
        i.fields
            .iter()
            .map(|f| (f.name.clone(), f.readonly))
            .collect()
    });
    let mut out = vec![];
    for k in cx.setter_fields(iface) {
        let (name, readonly) = &names[k];
        out.push(match slots.get(n_methods + k) {
            Some(Some(Slot::Value(j))) => SetterSrc::Field {
                d,
                self_ty,
                index: *j as usize,
                ty: field_tys[*j as usize].1,
            },
            _ => {
                let set = format!("set {name}");
                let found = field_tys.iter().position(|(n, _)| *n == set);
                let param = found.and_then(|j| match cx.ty.kind(field_tys[j].1) {
                    TyKind::FnPtr { params, .. } => params.last().copied(),
                    _ => None,
                });
                match (found, param) {
                    (Some(j), Some(param)) => SetterSrc::Call {
                        d,
                        self_ty,
                        index: j as u32,
                        fn_ty: field_tys[j].1,
                        param,
                        this: this_of(j as u32),
                    },
                    _ => match getter_ret(cx, slots.get(n_methods + k), field_tys) {
                        Some(ty) if *readonly => SetterSrc::Unwritable {
                            span,
                            name: name.clone(),
                            d,
                            self_ty,
                            ty,
                        },
                        _ => SetterSrc::Missing {
                            span,
                            name: name.clone(),
                        },
                    },
                }
            }
        });
    }
    out
}

/// The type a `get f()` in getter slot `slot` gives.
fn getter_ret(
    cx: &crate::ctx::Ctx,
    slot: Option<&Option<Slot>>,
    field_tys: &[(String, TyId)],
) -> Option<TyId> {
    let Some(Some(Slot::Call(j))) = slot else {
        return None;
    };
    match cx.ty.kind(field_tys.get(*j as usize)?.1) {
        TyKind::FnPtr { ret, .. } => Some(*ret),
        _ => None,
    }
}
