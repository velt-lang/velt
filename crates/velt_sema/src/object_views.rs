//! Conversions `crate::object_copies` finds a program could tell from a copy, in a program with
//! typed proxies: rewritten to the conversion through `any` a view comes from (velt_vir
//! lower/any_view.rs), which keeps the one object as Node does. And dynamic values holding an
//! `Object.freeze` result, which have no view of being frozen.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind, Intrinsic, LocalId, Stmt, StmtKind, TyId};
use crate::hir::TyKind;
use crate::visit::{self, VisitMut};
use crate::object_copies::{each_body, holds_by, related, Copy, Seen, Write};

/// Why a conversion can't stay a copy (`check`).
#[derive(Clone, Copy)]
pub(crate) enum Why<'w> {
    Write(&'w Write),
    Seen(TyId, Span, Seen),
}

/// Make the conversions `viewed` conversions through `any`, each of the object its fields were
/// read from (`AnyTo(to, ToDyn(Share(source)))`), and return those it found.
pub(crate) fn convert_to_views(cx: &mut Ctx, viewed: &[(&Copy, Why)]) -> FxHashSet<(Span, TyId)> {
    use crate::hir::{Conversion, ExprKind as E, UseMode};
    struct Mark<'a> {
        viewed: &'a [(&'a Copy, Why<'a>)],
        any: TyId,
        imp: u32,
        done: FxHashSet<(Span, TyId)>,
    }
    impl Mark<'_> {
        fn source(&self, e: &Expr) -> Option<Expr> {
            let E::AdtLit {
                converted: Conversion::Copy,
                ..
            } = &e.kind
            else {
                return None;
            };
            let at = |v: &&(&Copy, Why)| v.0.span == e.span && v.0.to == e.ty;
            self.viewed.iter().find(at)?.0.view_of.clone()
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
                    } if self.source(i).is_some() => Some(*local),
                    _ => None,
                });
                if let Some(l) = made {
                    b.stmts.retain(|s| !sets_field_of(s, l));
                }
                return;
            }
            let Some(mut src) = self.source(e) else {
                return;
            };
            self.done.insert((e.span, e.ty));
            // A conversion nested in this one's fields goes with it.
            if let E::AdtLit { fields, .. } = &mut e.kind {
                for f in fields.iter_mut() {
                    visit::expr(f, self);
                }
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

/// A write through `any` to the object a `const` made itself reaches no object a conversion copied
/// while the local's value has gone nowhere: as the receiver of a member read or write it stays
/// the local's, and a copy made after the write has what it wrote. So such writes count (go to
/// `writes`) only for a local whose value was used in another way before one of them, or in a
/// loop that has both.
pub(crate) fn fresh_writes_seen(
    cx: &mut Ctx,
    fresh: Vec<(LocalId, Span, Write)>,
    writes: &mut Vec<Write>,
) {
    if fresh.is_empty() {
        return;
    }
    let closures: FxHashMap<DefId, Vec<LocalId>> = (cx.defs.iter().enumerate())
        .filter_map(|(i, d)| match d {
            Some(Def::Fn(f)) => Some((
                DefId(i as u32),
                f.captures.iter().map(|c| c.outer).collect(),
            )),
            _ => None,
        })
        .collect();
    let mut seen: FxHashMap<(LocalId, Span), bool> = FxHashMap::default();
    for (local, decl, write) in fresh {
        let counts = *seen
            .entry((local, decl))
            .or_insert_with(|| fresh_local_shared(cx, local, decl, &closures));
        if counts {
            writes.push(write);
        }
    }
}

/// Was the value of `local`, declared at `decl`, used in another way before a write through it?
/// A local not found counts as shared.
fn fresh_local_shared(
    cx: &mut Ctx,
    local: LocalId,
    decl: Span,
    closures: &FxHashMap<DefId, Vec<LocalId>>,
) -> bool {
    for d in cx.defs.iter_mut().flatten() {
        let Def::Fn(f) = d else { continue };
        let mut uses = FreshUses::new(local, Some(decl), closures);
        visit::block(&mut f.body.block, &mut uses);
        if uses.declared {
            return uses.shared;
        }
    }
    true
}

/// The uses of a local, in source order (`fresh_writes_seen`).
struct FreshUses<'a> {
    local: LocalId,
    decl: Option<Span>,
    declared: bool,
    closures: &'a FxHashMap<DefId, Vec<LocalId>>,
    /// Receivers just seen, whose `Local` comes next in the walk.
    receivers: u32,
    gone: bool,
    wrote: bool,
    shared: bool,
}

impl<'a> FreshUses<'a> {
    fn new(
        local: LocalId,
        decl: Option<Span>,
        closures: &'a FxHashMap<DefId, Vec<LocalId>>,
    ) -> Self {
        FreshUses {
            local,
            decl,
            declared: false,
            closures,
            receivers: 0,
            gone: false,
            wrote: false,
            shared: false,
        }
    }
}

impl VisitMut for FreshUses<'_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            StmtKind::Let { local, .. } if *local == self.local && self.decl == Some(s.span) => {
                self.declared = true;
            }
            StmtKind::While { body, .. } | StmtKind::ForOf { body, .. } => {
                let mut inner = FreshUses::new(self.local, None, self.closures);
                visit::block(body, &mut inner);
                self.shared |= inner.gone && inner.wrote;
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            ExprKind::Call {
                callee: Callee::Intrinsic(i),
                args,
            } if reads_receiver(*i) => {
                let receiver = match args.first().map(|a| &a.kind) {
                    Some(ExprKind::Call {
                        callee: Callee::Intrinsic(Intrinsic::Share),
                        args,
                    }) => args.first().map(|a| &a.kind),
                    k => k,
                };
                if matches!(receiver, Some(ExprKind::Local(l, _)) if *l == self.local) {
                    self.receivers += 1;
                    if matches!(i, Intrinsic::AnySet | Intrinsic::AnySetIndex) {
                        self.wrote = true;
                        self.shared |= self.gone;
                    }
                }
            }
            ExprKind::Local(l, _) if *l == self.local => match self.receivers {
                0 => self.gone = true,
                _ => self.receivers -= 1,
            },
            ExprKind::Closure(c) => {
                self.gone |= (self.closures.get(c)).is_some_and(|o| o.contains(&self.local));
            }
            _ => {}
        }
    }
}

/// Does this intrinsic read or write through its first argument, and keep it nowhere?
fn reads_receiver(i: Intrinsic) -> bool {
    matches!(
        i,
        Intrinsic::AnyGet
            | Intrinsic::AnyIndex
            | Intrinsic::AnySet
            | Intrinsic::AnySetIndex
            | Intrinsic::AnyDelete
            | Intrinsic::AnyKeys
            | Intrinsic::AnyTruthy
            | Intrinsic::AnyTypeof
            | Intrinsic::AnyTag
    )
}
