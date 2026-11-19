//! The setter slots of an object literal implementing an interface (`iface_object`): how the
//! literal stores each field of the interface, for `crate::iface_writes`.

use velt_common::Span;

use super::iface_object::Slot;
use crate::hir::{self, DefId, TyId, TyKind};
use crate::iface_writes::SetterSrc;

/// How the literal stores each field of `iface` (`n_methods` methods), for the setter
/// slots of `crate::iface_writes`: a plain value in its field of struct `d`, a `get f()` through
/// the `set f(v)` stored next to it, or missing.
pub(super) fn literal_setters(
    cx: &mut crate::ctx::Ctx,
    (iface, args, n_methods): (DefId, &[TyId], usize),
    slots: &[Option<Slot>],
    field_tys: &[(String, TyId)],
    (d, self_ty, impl_index): (DefId, TyId, u32),
    this_of: &dyn Fn(u32) -> Option<(TyId, u32)>,
    span: Span,
) -> Vec<SetterSrc> {
    let uses_this: Vec<bool> = (0..field_tys.len() as u32)
        .map(|j| this_of(j).is_some())
        .collect();
    let msrcs = literal_method_sources(
        cx,
        (iface, args, n_methods),
        slots,
        field_tys,
        &uses_this,
        (d, self_ty),
    );
    cx.note_method_sources(impl_index, msrcs);
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

impl crate::body::FnCx<'_, '_> {
    /// An anonymous object literal's property `name` (`value`, `dynamic` as for
    /// `anon_prop_value`), field `index` of the literal (`body/expr/object.rs`): a function
    /// stored under the name of a method the program writes as a property is a site, so what is
    /// assigned to the method reaches it (`crate::iface_method_writes`). A site even while
    /// nothing reached it yet: the links to the method's site (`Ctx::link_method_sites`) are how
    /// anything does. Its field and site join `sites`, for `Ctx::note_method_field_sites`.
    pub(super) fn prop_value_site(
        &mut self,
        name: &velt_syntax::ast::Ident,
        value: Option<&velt_syntax::ast::Expr>,
        dynamic: bool,
        (index, sites): (usize, &mut Vec<(usize, Span)>),
    ) -> hir::Expr {
        let h = self.anon_prop_value(name, value, dynamic);
        let is_fn = matches!(self.cx.ty.kind(h.ty), TyKind::FnPtr { .. });
        if !is_fn || !self.cx.written_method_name(&name.name) {
            return h;
        }
        let Some(site) = self.cx.method_value_site(h.ty, name.span, self.module) else {
            return h;
        };
        let at = self.cx.site_in(name.span, site);
        self.flow_into(&h, at);
        sites.push((index, name.span));
        match site != h.ty {
            true => self.coerce(h, site),
            false => h,
        }
    }

    /// The literal's function values for methods the program writes as properties
    /// (`crate::iface_method_writes`), each made a site at its own span, so what is assigned to
    /// the method reaches the field storing it; with the values' field indices and sites.
    #[allow(clippy::type_complexity)] // the literal's values, as `iface_object` keeps them
    pub(super) fn method_value_sites(
        &mut self,
        mut values: Vec<(String, crate::hir::Expr, bool)>,
        methods: &[super::iface_object::MethodSig],
        slots: &[Option<Slot>],
    ) -> (Vec<(String, crate::hir::Expr, bool)>, Vec<(usize, Span)>) {
        let mut sites = vec![];
        for (j, m) in methods.iter().enumerate() {
            let Some(Some(Slot::Call(f))) = slots.get(j) else {
                continue;
            };
            let f = *f as usize;
            let Some((_, h, uses_this)) = values.get(f) else {
                continue;
            };
            let fn_value = matches!(self.cx.ty.kind(h.ty), TyKind::FnPtr { .. });
            if *uses_this || !fn_value || !self.cx.written_method_name(&m.name) {
                continue;
            }
            let at = h.span;
            let Some(site) = self.cx.method_value_site(h.ty, at, self.module) else {
                continue;
            };
            let target = self.cx.site_in(at, site);
            let h = values[f].1.clone();
            self.flow_into(&h, target);
            if site != h.ty {
                values[f].1 = self.coerce(h, site);
            }
            sites.push((f, at));
        }
        (values, sites)
    }
}

/// How an object literal written as an interface stores each method, should the program write
/// one through an interface value (`crate::iface_method_writes`): a function value that doesn't
/// use `this`, of the method's function type.
fn literal_method_sources(
    cx: &mut crate::ctx::Ctx,
    (iface, args, n_methods): (DefId, &[TyId], usize),
    slots: &[Option<Slot>],
    field_tys: &[(String, TyId)],
    uses_this: &[bool],
    (d, self_ty): (DefId, TyId),
) -> Vec<Option<crate::iface_method_writes::MethodSrc>> {
    let mut out = vec![];
    for j in 0..n_methods {
        let src = match slots.get(j).and_then(|s| s.as_ref()) {
            Some(Slot::Call(f)) if !uses_this.get(*f as usize).copied().unwrap_or(true) => {
                let ty = field_tys[*f as usize].1;
                (cx.stores_method(iface, args, j, ty))
                    .then_some(())
                    .map(|_| crate::iface_method_writes::MethodSrc {
                        d,
                        self_ty,
                        index: *f as usize,
                        ty,
                    })
            }
            _ => None,
        };
        out.push(src);
    }
    out
}
