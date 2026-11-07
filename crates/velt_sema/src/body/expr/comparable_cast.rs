//! `x as T` of an object whose type only overlaps `T` (TS2352's comparable relation): one type
//! has every required member of the other, and the members they share compare. These are the
//! assertions on objects TypeScript accepts that no other form covers: a target with optional
//! members the source lacks (`(ctx as { provides?: Map<symbol, unknown> }).provides`, `(i as {
//! dispose?: unknown } | null)?.dispose` of a `T`), or a source with fewer members (`{ name: "a" }
//! as Named`).
//!
//! TypeScript keeps the one object, and so does this: in a program with typed proxies (#823)
//! the cast is the conversion through `any` that a view comes from (velt_vir lower/any_view.rs),
//! whose reads and writes go to the object, a member's value checked as its type where it is
//! read: a required member the object lacks is a `TypeError` there, and a member its layout has
//! no room for (a marker written through the view) a `TypeError` at the write, where Node goes on.
//! Without typed proxies an object type holds only its own layout, and the cast stays an
//! error. A class target stays one too: its methods would run on another type's layout.
//!
//! `x as Record<K, V>` has no view: the record holds a copy of the fields, one
//! `crate::object_copies` reports once the program could tell it from the object.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};
use crate::object_copies::Copy;

/// How deep the comparison of member types goes before it takes them as comparable (recursive
/// types: TypeScript assumes the same of a pair it is already comparing).
const MAX_DEPTH: u32 = 6;

/// A member of an object type: its name, its type (`None`: a method), whether it is optional.
type Member = (String, Option<TyId>, bool);

impl FnCx<'_, '_> {
    /// `h as target` for an object `h` whose type is comparable to the object type `target`, or
    /// that one or `null` / `undefined`: a view of `h`'s object (module docs). `Err(h)` otherwise.
    pub(super) fn comparable_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let core = self.cx.ty.core(target).unwrap_or(target);
        // `{ href?: string } as Record<string, string | undefined>`: a record of its fields, as
        // through `any` (`x as any as Record<...>`).
        // A class's value doesn't compare with one (TS2352).
        let record = self.record_args(core).is_some()
            && self.cx.class_of(h.ty).is_none()
            && self.object_members(h.ty).is_some();
        let viewable = record || self.viewable(core);
        if !viewable || self.cx.ty.has_error(h.ty) {
            return Err(h);
        }
        if !record && !self.objects_overlap(h.ty, core, span, 0) {
            return Err(h);
        }
        if !record && self.method_seen_as_value(h.ty, core) {
            return Err(h);
        }
        let from = h.ty;
        let source = self.place_text(&h);
        let view = self.through_any(h, target)?;
        match record {
            true => self.note_record_copy(from, core, source, span),
            false => self.cx.object_copies.view_casts.push((span, core)),
        }
        Ok(view)
    }

    /// The record `to` holds a copy of the fields of a `from` (`Copy::record`).
    fn note_record_copy(&mut self, from: TyId, to: TyId, source: Option<String>, span: Span) {
        let from = self.cx.ty.core(from).unwrap_or(from);
        let members = self.object_members(from).unwrap_or_default();
        let fields = (members.into_iter())
            .filter(|m| m.1.is_some())
            .map(|m| m.0)
            .collect();
        self.cx.object_copies.copies.push(Copy {
            from,
            to,
            fields,
            absent: vec![],
            span,
            source,
            fresh: false,
            in_order: true,
            generic: false,
            record: true,
        });
    }

    /// An object `h` where the object type `exp` is expected, whose members its type has
    /// (TypeScript's assignability): an interface value where an object type with fewer or
    /// optional members is expected, an object where an interface is. It is a view too (module
    /// docs), in a program with typed proxies. `Err(h)` otherwise, and for a weak `exp` (all its
    /// members optional) that shares no member with `h`'s type (TS2559).
    pub(super) fn assignable_view(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let core = self.cx.ty.core(exp).unwrap_or(exp);
        let from = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        let nullable = self.cx.ty.core(h.ty).is_some();
        if (nullable && self.cx.ty.core(exp).is_none())
            || from == core
            || self.cx.ty.has_error(h.ty)
            || self.cx.ty.has_error(exp)
            || !self.viewable(core)
        {
            return Err(h);
        }
        let (Some(have), Some(want)) = (self.object_members(from), self.object_members(core))
        else {
            return Err(h);
        };
        if self.method_seen_as_value(from, core) {
            return Err(h);
        }
        let weak = want.iter().all(|m| m.2);
        let shares = want.iter().any(|m| have.iter().any(|n| n.0 == m.0));
        if weak && !shares {
            return Err(h);
        }
        let pair = (from, core);
        if !self.cx.object_copies.view_pairs.contains(&pair) {
            self.cx.object_copies.view_pairs.push(pair);
            let has = self.has_members(&have, &want, span, 0);
            self.cx.object_copies.view_pairs.pop();
            if !has {
                return Err(h);
            }
        }
        let view = self.through_any(h, exp)?;
        self.cx.object_copies.view_casts.push((span, core));
        Ok(view)
    }

    /// Is the brand `from` assignable to the brand `to`: `symbol & { t?: X[] }` to `symbol & { t?:
    /// unknown }`, a token of one type where a token of any type is expected? Brands are erased,
    /// so the value is the same. Each member must assign as it is: `"A"` is no `"B"`, which is
    /// what tells two brands of one primitive apart.
    pub(super) fn brand_assignable(&mut self, from: TyId, to: TyId, span: Span) -> bool {
        if from == to
            || self.cx.brand_base(from).is_none()
            || self.cx.brand_base(from) != self.cx.brand_base(to)
        {
            return false;
        }
        let (Some(a), Some(b)) = (self.cx.brand_tag(from), self.cx.brand_tag(to)) else {
            return false;
        };
        let (Some(have), Some(want)) = (self.object_members(a), self.object_members(b)) else {
            return false;
        };
        for (name, ty, optional) in &want {
            match (have.iter().find(|m| m.0 == *name), ty) {
                (None, _) if !*optional => return false,
                // An optional member where a required one is expected (TS2322).
                (Some((_, _, true)), _) if !*optional => return false,
                (Some((_, Some(x), _)), Some(y)) if !self.member_assigns(*x, *y, span) => {
                    return false;
                }
                _ => {}
            }
        }
        true
    }

    /// Does a value of type `from` convert to a `to`, as it is?
    fn member_assigns(&mut self, from: TyId, to: TyId, span: Span) -> bool {
        if from == to {
            return true;
        }
        // Types only: arrays compare by their elements, as TypeScript's do.
        let elements = |s: &Self, t: TyId| match s.cx.ty.kind(s.cx.ty.core(t).unwrap_or(t)) {
            TyKind::Array(e) => Some(*e),
            _ => None,
        };
        if let (Some(x), Some(y)) = (elements(self, from), elements(self, to)) {
            return self.member_assigns(x, y, span);
        }
        let mark = self.cx.object_copies.mark();
        let mut probe = self.error_expr(span);
        probe.ty = from;
        let converts = self.try_coerce(probe, to).is_ok();
        self.cx.object_copies.rollback(mark);
        converts
    }

    /// Would a view of a `from` as a `to` read a method as a value it then calls? A method read
    /// through `any` has no `this` yet (#823), and an object literal with methods is no value of
    /// another interface: `from` shows that these stop at run time, so they stay refused.
    fn method_seen_as_value(&mut self, from: TyId, to: TyId) -> bool {
        let from = self.cx.ty.core(from).unwrap_or(from);
        let (Some(have), Some(want)) = (self.object_members(from), self.object_members(to)) else {
            return false;
        };
        if let TyKind::Dyn(d, _) = self.cx.ty.kind(from) {
            if self.cx.literal_ifaces.contains(d) && matches!(self.cx.ty.kind(to), TyKind::Dyn(..))
            {
                return want.iter().any(|m| m.1.is_none());
            }
        }
        if matches!(self.cx.ty.kind(to), TyKind::Dyn(..)) {
            return false;
        }
        want.iter().any(|(name, ty, _)| {
            have.iter().any(|m| m.0 == *name && m.1.is_none())
                && ty.is_some_and(|t| {
                    let t = self.cx.ty.core(t).unwrap_or(t);
                    matches!(
                        self.cx.ty.kind(t),
                        TyKind::FnPtr { .. } | TyKind::Closure(_)
                    )
                })
        })
    }

    /// May a value of the object type `core` be a view of another object?
    fn viewable(&mut self, core: TyId) -> bool {
        match self.cx.ty.kind(core) {
            TyKind::Dyn(..) => {
                self.cx.dynamic(core).is_none()
                    && !self.cx.is_generic_fn(core)
                    && self.program_makes_proxies()
            }
            _ => self.cx.class_of(core).is_none() && self.may_be_typed_proxy(core),
        }
    }

    /// `h` converted to `target` through `any`; `Err(h)` when one of the two steps doesn't hold.
    fn through_any(&mut self, h: hir::Expr, target: TyId) -> Result<hir::Expr, hir::Expr> {
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let shown = h.clone();
        let boxed = match self.generic_as_any(h) {
            Ok(b) => b,
            Err(h) => {
                let h = self.share_if_place(h);
                match self.box_dynamic(h, any) {
                    Ok(b) => b,
                    Err(_) => return Err(shown),
                }
            }
        };
        self.coerce_dynamic(boxed, target, true).map_err(|_| shown)
    }

    /// Is a value of type `a` comparable to a `b`, in either direction (module docs)?
    fn objects_overlap(&mut self, a: TyId, b: TyId, span: Span, depth: u32) -> bool {
        if a == b || depth > MAX_DEPTH || self.cx.object_copies.view_pairs.contains(&(a, b)) {
            return true;
        }
        self.cx.object_copies.view_pairs.push((a, b));
        let overlap = self.objects_overlap_at(a, b, span, depth);
        self.cx.object_copies.view_pairs.pop();
        overlap
    }

    /// [`Self::objects_overlap`], with the pair `a`, `b` being compared.
    fn objects_overlap_at(&mut self, a: TyId, b: TyId, span: Span, depth: u32) -> bool {
        let open = |s: &Self, t: TyId| {
            s.cx.dynamic(t).is_some() || matches!(s.cx.ty.kind(t), TyKind::Param(_))
        };
        if open(self, a) || open(self, b) {
            return true;
        }
        let parts = |s: &mut Self, t: TyId| {
            let core = s.cx.ty.core(t).unwrap_or(t);
            s.cx.union_members(core).unwrap_or_else(|| vec![core])
        };
        let (pa, pb) = (parts(self, a), parts(self, b));
        if pa != [a] || pb != [b] {
            let pairs: Vec<(TyId, TyId)> = (pa.iter())
                .flat_map(|&x| pb.iter().map(move |&y| (x, y)))
                .collect();
            return (pairs.into_iter()).any(|(x, y)| self.objects_overlap(x, y, span, depth + 1));
        }
        match (self.object_members(a), self.object_members(b)) {
            (Some(ma), Some(mb)) => {
                self.has_members(&ma, &mb, span, depth) || self.has_members(&mb, &ma, span, depth)
            }
            _ => self.member_converts(a, b, span),
        }
    }

    /// Does an object with `from` give each member of `to`, of a comparable type, where it has
    /// it? An optional member may be missing.
    fn has_members(&mut self, from: &[Member], to: &[Member], span: Span, depth: u32) -> bool {
        to.iter().all(
            |(name, ty, optional)| match from.iter().find(|(n, ..)| n == name) {
                None => *optional,
                Some((_, have, _)) => match (have, ty) {
                    (Some(x), Some(y)) => self.objects_overlap(*x, *y, span, depth + 1),
                    _ => true,
                },
            },
        )
    }

    /// The members of an object type, a class's and an interface's (its own and inherited, and
    /// without private ones); `None` for any other type.
    fn object_members(&mut self, t: TyId) -> Option<Vec<Member>> {
        match self.cx.ty.kind(t).clone() {
            TyKind::Adt(d, args) => {
                if self.record_args(t).is_some() {
                    return None;
                }
                let a = self.cx.adt(d)?;
                let fields: Vec<(String, TyId, bool, bool)> = (a.fields.iter())
                    .map(|f| {
                        (
                            f.name.clone(),
                            f.declared,
                            f.optional,
                            f.private_to.is_some(),
                        )
                    })
                    .collect();
                let methods: Vec<String> = a.methods.keys().cloned().collect();
                let base = a.base;
                let mut out: Vec<Member> = vec![];
                for (name, ty, optional, private) in fields {
                    if !private {
                        out.push((name, Some(self.cx.subst(ty, &args)), optional));
                    }
                }
                out.extend(methods.into_iter().map(|m| (m, None, false)));
                if let Some(b) = base {
                    out.extend(self.object_members(b).unwrap_or_default());
                }
                Some(out)
            }
            TyKind::Dyn(d, args) => {
                let i = self.cx.iface(d)?;
                let fields: Vec<(String, TyId, bool)> = (i.fields.iter())
                    .map(|f| (f.name.clone(), f.declared, f.optional))
                    .collect();
                let methods: Vec<String> = i.methods.iter().map(|m| m.name.clone()).collect();
                let mut out: Vec<Member> = vec![];
                for (name, ty, optional) in fields {
                    out.push((name, Some(self.cx.subst(ty, &args)), optional));
                }
                out.extend(methods.into_iter().map(|m| (m, None, false)));
                Some(out)
            }
            _ => None,
        }
    }

    /// Does a value of one of the non-object types `a` and `b` convert to the other?
    fn member_converts(&mut self, a: TyId, b: TyId, span: Span) -> bool {
        // A view writes what its type holds: an `i32` seen as a `number` would lose a value.
        let (x, y) = (self.cx.widened(a), self.cx.widened(b));
        if x != y && self.cx.ty.is_numeric(x) && self.cx.ty.is_numeric(y) {
            return false;
        }
        let mark = self.cx.object_copies.mark();
        let same = self.view_member_comparable(a, b, span);
        self.cx.object_copies.rollback(mark);
        same
    }
}
