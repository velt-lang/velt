//! Conversions `crate::object_copies` finds a program could tell from a copy, in a program with
//! typed proxies: rewritten to the conversion through `any` a view comes from (velt_vir
//! lower/any_view.rs), which keeps the one object as Node does. And dynamic values holding an
//! `Object.freeze` result, which have no view of being frozen.

use rustc_hash::FxHashSet;
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Expr, TyId, TyKind};
use crate::object_copies::{each_body, holds_by, related, Copy, Frozen, Seen, Write};
use crate::visit::{self, VisitMut};

/// Why a conversion can't stay a copy (`check`).
#[derive(Clone, Copy)]
pub(crate) enum Why<'w> {
    Write(&'w Write),
    Seen(TyId, Span, Seen),
}

/// Make the conversions `viewed` conversions through `any`, each of the object its fields were
/// read from (`AnyTo(to, ToDyn(Share(source)))`), and return those it found.
pub(crate) fn convert_to_views(cx: &mut Ctx, viewed: &[(&Copy, Why)]) -> FxHashSet<(Span, TyId)> {
    use crate::hir::{
        Callee, Conversion, ExprKind as E, Intrinsic, LocalId, Stmt, StmtKind, UseMode,
    };
    struct Mark<'a> {
        viewed: &'a [(&'a Copy, Why<'a>)],
        any: TyId,
        imp: u32,
        done: FxHashSet<(Span, TyId)>,
        /// The source of the literal this block makes, read by the fields it sets after it.
        block_source: Option<Expr>,
    }
    /// The first `base.f` in `e` with `base` of type `from`: where the literal reads its fields.
    struct FieldOf {
        from: TyId,
        base: Option<Expr>,
    }
    impl VisitMut for FieldOf {
        fn expr(&mut self, e: &mut Expr) {
            if let E::Field { base, .. } = &e.kind {
                if base.ty == self.from && self.base.is_none() {
                    self.base = Some((**base).clone());
                }
            }
        }
    }
    impl Mark<'_> {
        fn copy_at(&self, e: &Expr) -> Option<&Copy> {
            let E::AdtLit {
                converted: Conversion::Copy,
                ..
            } = &e.kind
            else {
                return None;
            };
            let at = |v: &&(&Copy, Why)| v.0.span == e.span && v.0.to == e.ty;
            self.viewed.iter().find(at).map(|v| v.0)
        }
    }
    impl VisitMut for Mark<'_> {
        fn expr(&mut self, e: &mut Expr) {
            // The optional fields a copy sets after the literal (`copy_object`): a view has them.
            if let E::Block(b) = &mut e.kind {
                let made = b.stmts.iter().find_map(|s| match &s.kind {
                    StmtKind::Let {
                        local,
                        init: Some(i),
                    } => self.copy_at(i).map(|c| (*local, c.from)),
                    _ => None,
                });
                if let Some((l, from)) = made {
                    let mut find = FieldOf { from, base: None };
                    for s in b.stmts.iter_mut().filter(|s| sets_field_of(s, l)) {
                        visit::stmt(s, &mut find);
                    }
                    if find.base.is_some() || !b.stmts.iter().any(|s| sets_field_of(s, l)) {
                        b.stmts.retain(|s| !sets_field_of(s, l));
                    }
                    self.block_source = find.base;
                }
                // A literal that reads no field says so before it (`copy_object`).
                if let Some(from) = b
                    .value
                    .as_ref()
                    .and_then(|v| self.copy_at(v))
                    .map(|c| c.from)
                {
                    self.block_source = b.stmts.iter().find_map(|s| match &s.kind {
                        StmtKind::Expr(x) if x.ty == from => Some(x.clone()),
                        _ => None,
                    });
                }
                return;
            }
            let Some(from) = self.copy_at(e).map(|c| c.from) else {
                return;
            };
            let E::AdtLit { fields, .. } = &mut e.kind else {
                return;
            };
            let mut find = FieldOf { from, base: None };
            for f in fields.iter_mut() {
                visit::expr(f, &mut find);
            }
            let Some(mut src) = find.base.or_else(|| self.block_source.take()) else {
                // No field of the source is read: it stays a copy, and the error it was.
                return;
            };
            self.done.insert((e.span, e.ty));
            // A conversion nested in this one's fields goes with it.
            for f in fields.iter_mut() {
                visit::expr(f, self);
            }
            crate::body::places::set_place_mode(&mut src, UseMode::Borrow);
            let (span, ty) = (src.span, src.ty);
            let share = E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args: vec![src],
            };
            let boxed = E::ToDyn {
                expr: Box::new(Expr {
                    kind: share,
                    ty,
                    span,
                }),
                impl_index: self.imp,
            };
            e.kind = E::Call {
                callee: Callee::Intrinsic(Intrinsic::AnyTo),
                args: vec![Expr {
                    kind: boxed,
                    ty: self.any,
                    span,
                }],
            };
        }
    }
    /// `if (…) l.f = …`, as `copy_object` writes an optional field the source has.
    fn sets_field_of(s: &Stmt, l: LocalId) -> bool {
        let StmtKind::If { then, .. } = &s.kind else {
            return false;
        };
        let [Stmt {
            kind: StmtKind::Expr(a),
            ..
        }] = &then.stmts[..]
        else {
            return false;
        };
        let E::Assign { place, .. } = &a.kind else {
            return false;
        };
        let E::Field { base, .. } = &place.kind else {
            return false;
        };
        matches!(&base.kind, E::Local(x, _) if *x == l)
    }
    let (Some(imp), Some(defs)) = (cx.any_impl(), cx.any) else {
        return FxHashSet::default();
    };
    let any = cx.ty.intern(TyKind::Dyn(defs.any, vec![]));
    let mut mark = Mark {
        viewed,
        any,
        imp,
        done: FxHashSet::default(),
        block_source: None,
    };
    each_body(cx, &mut mark);
    mark.done
}

/// `Object.freeze(o)` converted to a dynamic type where the program assigns a field of its name on
/// a dynamic value or an object type a dynamic value may be viewed as: Node's frozen object
/// refuses the write. A frozen object has no mark at run time (its type is erased to the plain
/// one), so this is reported here, as a write to a frozen type is.
pub(crate) fn check_frozen_dynamic(cx: &mut Ctx, seen: &[(TyId, Span, Seen)], writes: &[Write]) {
    if cx.objects.frozen_defs.is_empty() || writes.is_empty() {
        return;
    }
    let frozen: Vec<crate::hir::DefId> = cx.objects.frozen_defs.iter().copied().collect();
    for &(t, span, how) in seen {
        if !matches!(how, Seen::Dynamic(_)) {
            continue;
        }
        let mut found = None;
        let mut is = |cx: &mut Ctx, x: TyId| match cx.ty.kind(x) {
            TyKind::Adt(d, _) if frozen.contains(d) => {
                found = Some(*d);
                true
            }
            _ => false,
        };
        if !holds_by(cx, t, &mut is, &mut FxHashSet::default()) {
            continue;
        }
        let Some(names) = found.and_then(|d| cx.adt(d)).map(|a| {
            (a.fields.iter())
                .map(|f| f.name.clone())
                .collect::<Vec<String>>()
        }) else {
            continue;
        };
        let hit = writes.iter().find(|w| {
            (w.name.is_empty() || names.contains(&w.name))
                && (w.dynamic || w.ty.is_none_or(|ty| cx.class_of(ty).is_none()))
        });
        if let Some(w) = hit {
            let what = match w.name.is_empty() {
                true => "a field through `any`".to_string(),
                false => format!("field `{}`", cx.shown_name(&w.name)),
            };
            cx.error(
                Diagnostic::error(
                    format!("an `Object.freeze` result converted to a dynamic type, in a program that assigns {what}, is not supported yet"),
                    span,
                )
                .with_label(w.span, "assigned here")
                .with_note("Node refuses a write to a frozen object, and Velt can't tell at run time that a value is one: pass it where a `Readonly<T>` is expected"),
            );
        }
    }
}

/// Frozen objects seen as a type whose fields can be written (`Copies::frozen`): an
/// `Object.freeze` result where such a type is expected (`const d: Desc = Object.freeze({ ... })`),
/// or `Object.freeze(x)` of a value made before. Node throws when a frozen object's field is
/// written, and Velt has no mark on the object to tell at run time; so each is an error once the
/// program assigns a field of that name on a value of either type, through an interface or a
/// type parameter (which may be either), or through `any` when a value holding one becomes an
/// `any`. `Object.assign` and `Object.defineProperty` onto it are such assignments.
pub(crate) fn check_frozen(
    cx: &mut Ctx,
    frozen: &[Frozen],
    seen: &[(TyId, Span, Seen)],
    writes: &[Write],
) {
    for f in frozen {
        let names: Vec<String> = match cx.ty.kind(f.from).clone() {
            TyKind::Adt(d, _) => (cx.adt(d).into_iter())
                .flat_map(|a| a.fields.iter().map(|x| x.name.clone()))
                .collect(),
            _ => continue,
        };
        let mut dynamic: Option<bool> = None;
        let hit = writes.iter().find(|w| {
            (w.name.is_empty() || names.contains(&w.name))
                && w.ty
                    .is_none_or(|t| related(cx, t, f.from) || related(cx, t, f.to))
                && (!w.dynamic
                    || *dynamic.get_or_insert_with(|| {
                        seen.iter().any(|&(t, _, how)| {
                            let mut is = |cx: &mut Ctx, x: TyId| {
                                related(cx, x, f.from) || related(cx, x, f.to)
                            };
                            matches!(how, Seen::Dynamic(_))
                                && holds_by(cx, t, &mut is, &mut FxHashSet::default())
                        })
                    }))
        });
        let Some(w) = hit else { continue };
        let what = match w.name.is_empty() {
            true => "a field whose name it knows only at run time".to_string(),
            false => format!("field `{}`", cx.shown_name(&w.name)),
        };
        let shown = cx.display(f.to);
        let d = match f.existing {
            true => Diagnostic::error(
                format!("`Object.freeze` of a `{shown}`, in a program that assigns {what}, is not supported yet"),
                f.span,
            )
            .with_note("Node throws a `TypeError` when a frozen object's field is written, and Velt can't tell at run time that a value is frozen: freeze an object literal, `Object.freeze({ ... })`, or use a `Readonly<T>` type"),
            false => Diagnostic::error(
                format!("an `Object.freeze` result where a `{shown}` is expected, in a program that assigns {what}, is not supported yet"),
                f.span,
            )
            .with_note("Node throws a `TypeError` when a frozen object's field is written, and Velt can't tell at run time that a value is frozen: pass it where a `Readonly<T>` is expected"),
        };
        cx.error(d.with_label(w.span, "assigned here"));
    }
}

/// A spread or rest of a type a cast makes a view of another object: Node copies all the object's
/// fields, and the fixed shape of the result has no place for the others. A cast the program had
/// to write through `any` keeps that limit (`any_view_spread`); this one is refused while no
/// program that compiled before depends on it.
pub(crate) fn check_view_spreads(
    cx: &mut Ctx,
    seen: &[(TyId, Span, Seen)],
    view_casts: &[(Span, TyId)],
) {
    if view_casts.is_empty() {
        return;
    }
    for &(t, at, how) in seen {
        if !matches!(how, Seen::Spread) {
            continue;
        }
        let t = cx.ty.core(t).unwrap_or(t);
        let Some(&(cast, to)) = view_casts.iter().find(|v| related(cx, t, v.1)) else {
            continue;
        };
        let shown = cx.display(to);
        cx.error(
            Diagnostic::error(
                format!("a spread of a `{shown}`, which a cast in this program makes a view of another object, is not supported yet"),
                at,
            )
            .with_label(cast, "the cast")
            .with_note("Node copies all the object's fields, and a value of this fixed shape has no place for the others: take the fields by name"),
        );
    }
}

/// A record cast of a type a cast makes a view of another object: a write to that object's own type
/// would not show as a write to the record's source.
pub(crate) fn check_record_sources(cx: &mut Ctx, copies: &[Copy], view_casts: &[(Span, TyId)]) {
    for c in copies.iter().filter(|c| c.record) {
        if view_casts.iter().any(|v| related(cx, c.from, v.1)) {
            let (from, to) = (cx.display(c.from), cx.display(c.to));
            cx.error(
                Diagnostic::error(
                    format!("a value of type `{from}` converts to `{to}` by copying its fields, and a cast in this program makes a `{from}` a view of another object, whose writes the record would not see"),
                    c.span,
                )
                .with_note("build the record with the fields you need (`{ a: x.a }`)"),
            );
        }
    }
}
