//! Arguments that read a place another use of the call changes: `f(s, (s = "x"))`,
//! `run(s, () => { s = "x"; })`, `r.rename(r.name)`. JavaScript reads the value when it evaluates
//! the argument, and the callee keeps it whatever happens to the place. A value it does not hold
//! by reference (a string, a union, an object type, `any`) then gets a reference of its own, made
//! in the argument's turn, so nothing the callee or a later argument does to the place reaches
//! the callee's parameter or frees what it reads. Class instances and arrays keep the rule:
//! there the conflict is about one object, not about what a place holds.

use velt_common::Span;

use crate::any::Dynamic;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId, Expr, ExprKind as E, TyId, TyKind, UseMode};

use super::uses::{Access, Collector, Place, Use};
use super::{modifies_capture, ArgUses};
use crate::ownership::soft::make_share;

/// Share each argument of `args` (its uses at `uses[i + first]`) that reads a place another
/// argument changes, and redo its uses. `identity`: class instances and arrays too.
pub(super) fn share_value_reads(
    col: &Collector<'_, '_>,
    args: &mut [Expr],
    uses: &mut ArgUses,
    first: usize,
    identity: bool,
) {
    let changes = |(direct, nested): &(Vec<Use>, Vec<Use>)| {
        !nested.is_empty() || direct.iter().any(|u| u.access != Access::Shared)
    };
    if !uses.iter().any(changes) {
        return;
    }
    for (i, a) in args.iter_mut().enumerate() {
        let k = i + first;
        let reads = uses[k].0.iter().any(|u| u.access == Access::Shared);
        if !reads {
            continue;
        }
        let clash = |p: &Place| {
            uses.iter()
                .enumerate()
                .filter(|(j, _)| *j != k)
                .any(|(j, (direct, nested))| {
                    let later = j > k;
                    let hit = |w: &Use| {
                        w.place.overlaps(p)
                            && w.place.proj.len() <= p.proj.len()
                            && replaces(col, w, later)
                    };
                    direct.iter().any(hit) || nested.iter().any(hit)
                })
        };
        if share_reads(col, a, identity, &clash) {
            let (mut direct, mut nested) = (vec![], vec![]);
            col.direct(a, &mut direct, &mut nested);
            uses[k] = (direct, nested);
        }
    }
}

/// `((v) => { s = "x"; return v; })(s)`: a call of a closure literal, whose body changes the
/// variables it captured while its params still point at them. Any kind of value: JavaScript's
/// callee keeps the object too, and a class instance the closure replaces would be freed.
pub(super) fn immediately_called(
    col: &Collector<'_, '_>,
    def: DefId,
    span: Span,
    args: &mut [Expr],
) {
    let mut caps = vec![];
    col.captures(def, span, true, &mut caps);
    if caps.is_empty() {
        return;
    }
    let mut uses: ArgUses = vec![(caps, vec![])];
    for a in args.iter_mut() {
        let (mut direct, mut nested) = (vec![], vec![]);
        col.direct(a, &mut direct, &mut nested);
        uses.push((direct, nested));
    }
    share_value_reads(col, args, &mut uses, 1, true);
}

/// Does use `w`, of the place a read sees or one around it, take away the value? A write does (a
/// write inside the value keeps the rule: a share of an object stored inline would not see it). A
/// move does when the read comes first (`later`), or when the variable is in a cell, which a
/// closure's capture only shares; a closure taking over a variable it only reads is
/// `share_borrowed_captures`' case.
fn replaces(col: &Collector<'_, '_>, w: &Use, later: bool) -> bool {
    match (w.access, w.capture) {
        (Access::Shared, _) => false,
        (Access::Unique, _) => true,
        (Access::Move, None) => later,
        (Access::Move, Some((c, outer))) => {
            col.locals[outer.0 as usize].boxed || later && modifies_capture(col.cx, c, outer, 0)
        }
    }
}

/// Share the places argument `e` passes that `clash` says another use changes.
fn share_reads(
    col: &Collector<'_, '_>,
    e: &mut Expr,
    identity: bool,
    clash: &dyn Fn(&Place) -> bool,
) -> bool {
    if let Some((p, mode)) = col.place_of(e) {
        let take = mode == UseMode::Borrow && clash(&p);
        if take && (identity || read_by_value(col.cx, e.ty)) {
            make_share(e);
            return true;
        }
        return false;
    }
    match &mut e.kind {
        E::Upcast(x) | E::WrapSome(x) | E::ToDyn { expr: x, .. } => {
            share_reads(col, x, identity, clash)
        }
        E::If { then, els, .. } => {
            let a = share_reads(col, then, identity, clash);
            share_reads(col, els, identity, clash) || a
        }
        _ => false,
    }
}

/// Is a value of type `t` one a place holds whole, with no class instance or array in it: a string,
/// a number, an `any`, or a union, tuple or object type of those? A reference to it keeps what the
/// place held whatever the place gets later; a part JavaScript holds by reference keeps the rule.
fn read_by_value(cx: &Ctx, t: TyId) -> bool {
    holds_values(cx, t, 0)
}

fn holds_values(cx: &Ctx, t: TyId, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    let all =
        |mut ts: Box<dyn Iterator<Item = TyId> + '_>| ts.all(|x| holds_values(cx, x, depth + 1));
    match cx.ty.kind(t) {
        TyKind::Str
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Bool
        | TyKind::Unit
        | TyKind::Literal(_) => true,
        TyKind::Dyn(..) => matches!(cx.dynamic(t), Some(Dynamic::Any | Dynamic::Unknown)),
        TyKind::Option(x, _) => holds_values(cx, *x, depth + 1),
        // One promise, shared: replacing the place leaves it as it is.
        TyKind::Promise(..) => true,
        TyKind::Tuple(xs) => all(Box::new(xs.iter().copied())),
        TyKind::Adt(d, _) => match (cx.adt(*d), cx.enum_info(*d)) {
            (Some(a), _) => {
                a.kind != AdtKind::Class
                    && !a.has_dispose
                    && all(Box::new(a.fields.iter().map(|f| f.ty)))
            }
            (None, Some(e)) => all(Box::new(
                e.variants.iter().flat_map(|v| v.payload.iter().copied()),
            )),
            _ => false,
        },
        _ => false,
    }
}
