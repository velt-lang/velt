//! Union views (#1410; hir_encodings.md "Union views"): a union whose members are all
//! interfaces with methods (sigx's `AsyncState<T>`) can also hold one interface value whose
//! discriminant changes, as a wide object with getters cast to the union
//! (`impl as AsyncState<T>`).
//!
//! The union enum gets one more variant, `<view>`, after the members' (which keep their
//! variants: an object literal built as a member stays that member, with its tag). Its payload is
//! an interface value of the union's *view interface*: an interface sema makes that extends the
//! members' nearest common interface (their shared methods) and has, readonly, every field all
//! members have, typed as the union of the members' types (`state: 'idle' | 'ready' | …`). A
//! wide interface value whose members include all of the view interface's, with the same types,
//! converts to it with an `Upcast` (the same object), so the view reads the object's own getters
//! on every access.
//!
//! Every use that matches on the union gives the `<view>` variant an arm of its own that does the
//! same through the view interface: a field read calls the getter (and checks the value against
//! the members flow narrowing left), a discriminant test compares the live value, a `switch`
//! matches the live value. A use with no such arm is a compile error once the program makes a
//! view of that union ([`Ctx::union_view_errors`]); lowering also panics on a view that reaches a
//! match without an arm or a narrowed read of one member. Programs that never cast to the union
//! lose the arms again ([`strip_unused`]), so their unions lower as before.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{self, Def, DefId, PatKind, TyId, TyKind};
use crate::visit::{self, VisitMut};

/// The view of one union definition.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ViewInfo {
    /// The view interface value type, over the union's own type parameters.
    pub iface: TyId,
    /// The view interface itself: one per *family* of unions (the members' interfaces, at any
    /// arguments), so a generic union's instance and the union sema made for the same members
    /// hold the same type (lowering maps one onto the other, `Program::union_shapes`).
    pub family: DefId,
    /// The `<view>` variant: after the members'.
    pub variant: u32,
}

/// The union views of a program.
#[derive(Default)]
pub(crate) struct UnionViews {
    by_def: FxHashMap<DefId, Option<ViewInfo>>,
    /// The view interfaces (families) a view value is made of.
    made: FxHashSet<DefId>,
    /// Uses of a union value that have no `<view>` arm: union, where, why.
    unsupported: Vec<(DefId, Span, String)>,
    /// Narrowed reads of one member (`UnwrapVariant`) of a local that holds that member because
    /// it was assigned to it.
    safe: FxHashSet<Span>,
}

/// The name of the `<view>` variant, which lowering recognizes.
pub(crate) const VIEW_VARIANT: &str = "<view>";

impl Ctx<'_> {
    /// The view of union definition `d`, made the first time it is asked for (`None`: the union
    /// has none: a member is not an interface with methods, or the members share no interface).
    pub(crate) fn union_view_of(&mut self, d: DefId) -> Option<ViewInfo> {
        if let Some(v) = self.union_views.by_def.get(&d) {
            return *v;
        }
        if !self.synth_ifaces_flattened() {
            return None;
        }
        let v = self.make_union_view(d);
        self.union_views.by_def.insert(d, v);
        v
    }

    /// The view of union type `u` at its arguments: (union def, `<view>` variant, view type).
    pub(crate) fn union_view(&mut self, u: TyId) -> Option<(DefId, u32, TyId)> {
        let (d, args) = self.union_def(u)?;
        let v = self.union_view_of(d)?;
        Some((d, v.variant, self.subst(v.iface, &args)))
    }

    /// Records that a view of union `d` is made: its family's unions get the `<view>` variant.
    pub(crate) fn union_view_made(&mut self, d: DefId) {
        if let Some(v) = self.union_view_of(d) {
            self.union_views.made.insert(v.family);
        }
    }

    /// Records that the narrowed member read at `span` reads a member value, never a view.
    pub(crate) fn union_view_safe(&mut self, span: Span) {
        self.union_views.safe.insert(span);
    }

    /// Records a use of union `d` that has no `<view>` arm (an error once a view is made).
    pub(crate) fn union_view_unsupported(&mut self, d: DefId, span: Span, what: &str) {
        let what = what.to_string();
        self.union_views.unsupported.push((d, span, what));
    }

    /// The view of union `d` when a view of its family is made.
    fn view_made(&self, d: DefId) -> Option<ViewInfo> {
        let v = (*self.union_views.by_def.get(&d)?)?;
        self.union_views.made.contains(&v.family).then_some(v)
    }

    /// The `<view>` variant of union `d` when the program made a view of its family (finalize
    /// adds it): its index and payload type.
    pub(crate) fn union_view_variant(&self, d: DefId) -> Option<(u32, TyId)> {
        self.view_made(d).map(|v| (v.variant, v.iface))
    }

    /// Errors for the uses with no `<view>` arm of the unions the program made views of.
    pub(crate) fn union_view_errors(&mut self) {
        let mut seen = FxHashSet::default();
        for (d, span, what) in std::mem::take(&mut self.union_views.unsupported) {
            if self.view_made(d).is_none() || !seen.insert((span, what.clone())) {
                continue;
            }
            self.error(
                Diagnostic::error(
                    format!("{what} is not supported yet on a union that holds a view"),
                    span,
                )
                .with_note("a value cast to this union (`x as U`) is read through its getters; this use would need to know its member (#1410)"),
            );
        }
    }

    fn make_union_view(&mut self, d: DefId) -> Option<ViewInfo> {
        let e = self.enum_info(d)?;
        if !e.is_union {
            return None;
        }
        let members: Vec<TyId> = e.variants.iter().map(|v| v.payload[0]).collect();
        if members.len() < 2 || !members.iter().all(|&m| self.is_method_iface(m)) {
            return None;
        }
        // The family: the members' interfaces in `DefId` order, each over parameters of its own
        // (one family whatever the union's arguments and member order).
        let mut heads: Vec<(DefId, Vec<TyId>)> = vec![];
        for &m in &members {
            heads.push(self.iface_member(m)?);
        }
        heads.sort_by_key(|(i, _)| i.0);
        if heads.windows(2).any(|w| w[0].0 == w[1].0) {
            return None;
        }
        let mut generic = vec![];
        let mut args = vec![];
        for (i, a) in &heads {
            let n = self.iface(*i)?.generics.len();
            if n != a.len() {
                return None;
            }
            let own: Vec<TyId> = (0..n)
                .map(|j| self.ty.param((args.len() + j) as u32))
                .collect();
            args.extend(a.iter().copied());
            generic.push(self.ty.intern(TyKind::Dyn(*i, own)));
        }
        let common = self.common_iface(&generic)?;
        let module = self.iface(heads[0].0).map_or(0, |i| i.module);
        let names: Vec<String> = self
            .iface(heads[0].0)
            .map(|i| i.fields.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default();
        let mut fields = vec![];
        for name in names {
            let mut tys = vec![];
            for &m in &generic {
                match self.member_field_ty(m, &name) {
                    Some(t) => tys.push(t),
                    None => break,
                }
            }
            if tys.len() < generic.len() {
                continue;
            }
            let ty = self.view_field_ty(&tys);
            if ty == self.ty.error {
                continue;
            }
            fields.push(ShapeField {
                name,
                ty,
                readonly: true,
                optional: false,
            });
        }
        let object = self.anon_type_with(&fields, module);
        let iface = match self.meet_iface(common, object, module, Span::DUMMY)? {
            Ok(t) => t,
            Err(_) => return None,
        };
        let TyKind::Dyn(family, _) = *self.ty.kind(iface) else {
            return None;
        };
        Some(ViewInfo {
            iface: self.subst(iface, &args),
            family,
            variant: members.len() as u32,
        })
    }

    /// The union of the members' types of one field: `true | false` is `boolean`.
    fn view_field_ty(&mut self, tys: &[TyId]) -> TyId {
        let bools: Vec<Option<bool>> = tys
            .iter()
            .map(|&t| match self.lit_value(t) {
                Some(hir::LitValue::Bool(b)) => Some(b),
                _ => None,
            })
            .collect();
        if bools.iter().all(|b| b.is_some())
            && bools.contains(&Some(true))
            && bools.contains(&Some(false))
        {
            return self.ty.bool_;
        }
        self.union_of(tys, false, Span::DUMMY)
    }

    /// The nearest interface every member extends (or is, by interface) that declares
    /// methods, as the first member has it.
    fn common_iface(&mut self, members: &[TyId]) -> Option<TyId> {
        let lines: Vec<Vec<TyId>> = members.iter().map(|&m| self.ancestors_of(m)).collect();
        let head = |cx: &Self, t: TyId| cx.iface_member(t).map(|(i, _)| i);
        lines[0].iter().copied().find(|&a| {
            let h = head(self, a);
            lines[1..]
                .iter()
                .all(|l| l.iter().any(|&b| head(self, b) == h))
                && h.and_then(|i| self.iface(i))
                    .is_some_and(|i| !i.methods.is_empty())
        })
    }

    /// Interface type `t` and the interfaces it extends (with their arguments), nearest first.
    fn ancestors_of(&mut self, t: TyId) -> Vec<TyId> {
        let mut out = vec![t];
        let mut i = 0;
        while i < out.len() && out.len() < 64 {
            let Some((d, args)) = self.iface_member(out[i]) else {
                i += 1;
                continue;
            };
            let parents = self.iface(d).map(|f| f.parents.clone()).unwrap_or_default();
            for p in parents {
                let pargs: Vec<TyId> = p.args.iter().map(|&a| self.subst(a, &args)).collect();
                let pt = self.ty.intern(TyKind::Dyn(p.iface, pargs));
                if !out.contains(&pt) {
                    out.push(pt);
                }
            }
            i += 1;
        }
        out
    }
}

/// Once every body is checked: removes the `<view>` arms of the unions no view is made of; for
/// the unions with views, reports each match without a `<view>` arm (other than one binding the
/// whole value) and each narrowed read of one member (`UnwrapVariant`), which would read a view
/// as another member.
pub(crate) fn finish(cx: &mut Ctx) {
    if cx.union_views.by_def.is_empty() {
        return;
    }
    let views: FxHashMap<DefId, (u32, bool)> = cx
        .union_views
        .by_def
        .iter()
        .filter_map(|(&d, v)| v.map(|v| (d, (v.variant, cx.union_views.made.contains(&v.family)))))
        .collect();
    let mut pass = Arms {
        views: &views,
        safe: &cx.union_views.safe,
        ty: &cx.ty,
        bad: vec![],
    };
    for def in cx.defs.iter_mut().flatten() {
        match def {
            Def::Fn(f) => visit::block(&mut f.body.block, &mut pass),
            Def::Global(g) => visit::expr(&mut g.init, &mut pass),
            _ => {}
        }
    }
    let bad = std::mem::take(&mut pass.bad);
    for (d, span, what) in bad {
        cx.union_view_unsupported(d, span, what);
    }
    cx.union_view_errors();
}

struct Arms<'a> {
    views: &'a FxHashMap<DefId, (u32, bool)>,
    safe: &'a FxHashSet<Span>,
    ty: &'a crate::types::Types,
    bad: Vec<(DefId, Span, &'static str)>,
}

impl Arms<'_> {
    /// The union with views that `t` (or its payload, for `U | null`) is: def, variant, made.
    fn view_of(&self, t: TyId) -> Option<(DefId, u32, bool)> {
        let t = self.ty.core(t).unwrap_or(t);
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => self.views.get(d).map(|&(v, m)| (*d, v, m)),
            _ => None,
        }
    }
}

impl VisitMut for Arms<'_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        match &mut e.kind {
            hir::ExprKind::Match { scrutinee, arms } => {
                let Some((d, variant, made)) = self.view_of(scrutinee.ty) else {
                    return;
                };
                if !made {
                    arms.retain(|a| !is_view_pat(&a.pat, d, variant));
                    return;
                }
                let covered = arms.iter().any(|a| {
                    is_view_pat(&a.pat, d, variant)
                        || matches!(a.pat.kind, PatKind::Binding(..)) && a.guard.is_none()
                });
                // A match on `U | null` tests `null` first, its payload inside `Some`.
                let nested = self.ty.core(scrutinee.ty).is_some();
                if !covered && !nested {
                    self.bad.push((d, e.span, "this use of the union value"));
                }
            }
            hir::ExprKind::UnwrapVariant { expr, .. } => {
                let safe = self.safe.contains(&e.span);
                if let (Some((d, _, true)), false) = (self.view_of(expr.ty), safe) {
                    self.bad
                        .push((d, e.span, "using a narrowed union value as its member"));
                }
            }
            _ => {}
        }
    }
}

/// Is `p` (an `Or` of) the `<view>` variant of `d`?
fn is_view_pat(p: &hir::Pat, d: DefId, variant: u32) -> bool {
    match &p.kind {
        PatKind::Variant {
            def, variant: v, ..
        } => *def == d && *v == variant,
        PatKind::Or(ps) => ps.iter().any(|p| is_view_pat(p, d, variant)),
        _ => false,
    }
}
