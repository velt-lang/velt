//! Appending to a string variable or field in place: `s += x`, `s = s + x` and
//! `` s = `${s}${x}` `` (sema lowers all three to `s = StrConcat(s, …)`). The old value of `s` is
//! dead after the assignment, so the new text goes onto the end of it with
//! `velt_rt_str_append` (amortized O(1): in place when `s` holds the only reference to its
//! buffer, which grows geometrically) instead of copying the whole string every time, which made
//! a loop of appends quadratic.
//!
//! The text appended (everything after the leading read of `s`) is evaluated first, into a
//! string of its own when it has several parts, and `s` is only touched once it is complete: a
//! part that throws leaves `s` unchanged, as in JS. When that text may change `s` itself (it
//! reads `s`, or calls code that could reach it), `s` keeps its old value alive while the text
//! is evaluated (one count increment), as JS reads `s` before the right-hand side runs.

use velt_sema::hir::{self, Callee, Intrinsic, LocalId, TyId, TyKind};

use super::rt::Rt;
use super::template::{flatten, Part};
use super::{unit, FnLower};
use crate::vir::{Operand, Place, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

/// What the appended text may do to the target without the old value being kept alive.
struct Target<'e> {
    place: &'e hir::Expr,
    /// Direct calls are allowed in the text: the target is a plain local no other code can
    /// reach (not captured by a closure, not a parameter written in place).
    calls_ok: bool,
}

impl FnLower<'_, '_> {
    /// `place = value` as an in-place append when `value` is a concatenation that starts with a
    /// read of the string `place` (a local, or a field path rooted at one). `None` (nothing
    /// emitted) for any other assignment.
    pub(super) fn str_append(&mut self, place: &hir::Expr, value: &hir::Expr) -> Option<Operand> {
        let hir::ExprKind::Call {
            callee: Callee::Intrinsic(Intrinsic::StrConcat),
            ..
        } = &value.kind
        else {
            return None;
        };
        let mut parts = vec![];
        flatten(value, &mut parts);
        let (Part::Str(read), rest) = parts.split_first()? else {
            return None;
        };
        if !same_place(read, place) {
            return None;
        }
        self.append_parts(place, rest)
    }

    /// `place += value` on a string, as an in-place append (`None`: not a string target this
    /// applies to, nothing emitted).
    pub(super) fn str_append_compound(
        &mut self,
        place: &hir::Expr,
        value: &hir::Expr,
    ) -> Option<Operand> {
        let mut parts = vec![];
        flatten(value, &mut parts);
        self.append_parts(place, &parts)
    }

    /// Append the text of `rest` to the string `place`, if it owns its value.
    fn append_parts(&mut self, place: &hir::Expr, rest: &[Part]) -> Option<Operand> {
        let ty = self.sub(place.ty);
        if !matches!(self.cx.kind(ty), TyKind::Str) {
            return None;
        }
        let target = self.append_target(place)?;
        let alone = rest.iter().all(|p| match p {
            Part::Text(_) => true,
            Part::Str(e) | Part::Format(e) => leaves_alone(e, &target),
        });
        Some(match alone {
            true => self.append_now(place, rest, ty),
            false => self.append_keeping_old(place, rest, ty),
        })
    }

    /// Can `place` be appended to in place: does it own its string, the way an assignment
    /// drops the old value? Fields moved out of a local (re-initialized by the assignment) and
    /// locals that only borrow their value are not.
    fn append_target<'e>(&self, place: &'e hir::Expr) -> Option<Target<'e>> {
        match &place.kind {
            hir::ExprKind::Local(id, _) => {
                let info = &self.info[id.0 as usize];
                info.vir?;
                let owned = info.droppable && !info.cell;
                (owned || info.indirect).then_some(Target {
                    place,
                    calls_ok: owned && !info.indirect,
                })
            }
            hir::ExprKind::Field { base, index, .. } => {
                if let hir::ExprKind::Local(id, _) = &base.kind {
                    if self.info[id.0 as usize].moved_fields.contains(index) {
                        return None;
                    }
                }
                Some(Target {
                    place,
                    calls_ok: false,
                })
            }
            _ => None,
        }
    }

    /// The appended text cannot change the target: evaluate it, then append. Parts that cannot
    /// fail half-way go straight onto the end of the target (no string in between).
    fn append_now(&mut self, place: &hir::Expr, rest: &[Part], ty: TyId) -> Operand {
        if rest.len() > 1 && rest.iter().all(|p| !may_fail(p)) {
            let p = self.place_expr(place);
            let bp = self.addr(p);
            self.push_parts(&bp, rest);
            return unit();
        }
        let text = match rest {
            [Part::Text(s)] => self.str_lit(s),
            [Part::Str(e)] => self.expr(e),
            _ => self.build_parts(rest, ty),
        };
        if self.dead() {
            return unit();
        }
        let text = self.operand_addr(text, STR);
        let p = self.place_expr(place);
        self.append_to(p, text);
        unit()
    }

    /// The appended text may change the target: share its old value first, evaluate the text
    /// into a string of its own, then put the old value back (dropping whatever the text left
    /// there; the count is back to one when it left the target alone) and append.
    fn append_keeping_old(&mut self, place: &hir::Expr, rest: &[Part], ty: TyId) -> Operand {
        let first = self.place_expr(place);
        let old = self.share_value(Operand::Copy(first), ty);
        let old = self.own_value(old, ty);
        let text = self.build_parts(rest, ty);
        if self.dead() {
            return unit();
        }
        let text = self.operand_addr(text, STR);
        let old = self.take_owned(old);
        let p = match &place.kind {
            hir::ExprKind::Local(id, _) => {
                let p = self.place_expr(place);
                self.drop_old(*id);
                self.store(p.clone(), old);
                self.mark_init(*id);
                p
            }
            _ => {
                // Formed again: the text may have replaced objects along the path.
                let p = self.place_expr(place);
                let prev = self.copy_to_temp(Operand::Copy(p.clone()), STR);
                self.store(p.clone(), old);
                self.drop_glue(Place::local(prev), ty);
                p
            }
        };
        self.append_to(p, text);
        unit()
    }

    /// `velt_rt_str_append(&p, text)`.
    fn append_to(&mut self, p: Place, text: Operand) {
        let pa = self.addr(p);
        self.call_rt(Rt::StrAppend, vec![pa, text], None);
    }
}

/// Is `read` (a string read, possibly shared) the same variable or field path as `place`?
fn same_place(read: &hir::Expr, place: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match (&read.kind, &place.kind) {
        (
            K::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            },
            _,
        ) if args.len() == 1 => same_place(&args[0], place),
        (K::Local(a, _), K::Local(b, _)) => a == b,
        (
            K::Field {
                base: rb,
                index: ri,
                ..
            },
            K::Field {
                base: pb,
                index: pi,
                ..
            },
        ) => ri == pi && same_place(rb, pb),
        _ => false,
    }
}

/// Can evaluating `e` neither read nor change the target? Conservative: plain reads, operators
/// and string formatting, plus direct calls when the target is a local no callee can reach.
fn leaves_alone(e: &hir::Expr, t: &Target) -> bool {
    use hir::ExprKind as K;
    let all = |es: &[hir::Expr]| es.iter().all(|e| leaves_alone(e, t));
    match &e.kind {
        K::Lit(_) => true,
        K::Local(id, _) => !is_local(t.place, *id),
        K::Field { base, index, .. } => {
            let same_field = matches!(&t.place.kind, K::Field { index: i, .. } if i == index);
            !same_field && leaves_alone(base, t)
        }
        K::Index { base, index, .. } => leaves_alone(base, t) && leaves_alone(index, t),
        K::Unary { expr, .. } | K::Cast(expr) | K::Upcast(expr) => leaves_alone(expr, t),
        K::Binary { lhs, rhs, .. } => leaves_alone(lhs, t) && leaves_alone(rhs, t),
        K::If { cond, then, els } => {
            leaves_alone(cond, t) && leaves_alone(then, t) && leaves_alone(els, t)
        }
        K::Call {
            callee: Callee::Intrinsic(Intrinsic::ToString | Intrinsic::StrConcat | Intrinsic::Share),
            args,
        } => all(args),
        K::Call {
            callee: Callee::Def(..),
            args,
        } => t.calls_ok && all(args),
        _ => false,
    }
}

/// Can evaluating `part` throw or leave the function (so that a text appended part by part
/// would be left half-appended)? Conservative: only plain reads, formatting and arithmetic
/// that cannot divide by zero are known not to.
fn may_fail(part: &Part) -> bool {
    match part {
        Part::Text(_) => false,
        Part::Str(e) | Part::Format(e) => !infallible(e),
    }
}

fn infallible(e: &hir::Expr) -> bool {
    use hir::ExprKind as K;
    match &e.kind {
        K::Lit(_) | K::Local(..) => true,
        K::Field { base, .. } => infallible(base),
        K::Unary { expr, .. } | K::Upcast(expr) => infallible(expr),
        K::Binary { op, lhs, rhs } => {
            !matches!(op, hir::BinOp::Div | hir::BinOp::Rem) && infallible(lhs) && infallible(rhs)
        }
        K::Call {
            callee: Callee::Intrinsic(Intrinsic::ToString | Intrinsic::Share),
            args,
        } => args.iter().all(infallible),
        _ => false,
    }
}

/// Is `place` the local `id`?
fn is_local(place: &hir::Expr, id: LocalId) -> bool {
    matches!(place.kind, hir::ExprKind::Local(l, _) if l == id)
}
