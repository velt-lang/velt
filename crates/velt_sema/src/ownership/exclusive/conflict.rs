//! Which two argument uses of a call conflict, and the diagnostic for the first pair that does.

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::TyKind;

use super::uses::{shown, Access, Use};
use super::ArgUses;

/// `skip`: a pair of direct uses that is no conflict.
pub(super) fn first_conflict(
    uses: &ArgUses,
    skip: &dyn Fn(&Use, &Use) -> bool,
) -> Option<Diagnostic> {
    for (i, (direct_i, _)) in uses.iter().enumerate() {
        for (j, (direct_j, nested_j)) in uses.iter().enumerate() {
            if i == j {
                continue;
            }
            for u in direct_i {
                if i < j {
                    let hit = direct_j.iter().find(|w| clash(u, w) && !skip(u, w));
                    if let Some(w) = hit {
                        return Some(report_direct(u, w));
                    }
                }
                let by_ref = matches!(u.access, Access::Shared | Access::Unique);
                let hit = nested_j
                    .iter()
                    .find(|w| by_ref && u.place.overlaps(&w.place) && !changes_inside(u, w));
                if let Some(w) = hit {
                    return Some(report(u, w));
                }
            }
        }
    }
    None
}

/// Does `w`, a change made while another argument is evaluated, only change the inside of
/// the value `u` hands to the callee by reference (`this.set(this.src.get(1))`, `get`
/// modifying `this.src`)? JavaScript's callee then sees the change too: no conflict. So is a
/// write through another reference to the passed place itself (`g(h.vs, (h.vs.length = 0))`,
/// through a share of `h.vs`): it changes the array `h.vs` holds, not which array that is.
/// Replacing the value or a place on its way (`f(x.arr, reset(x))`) still is one.
fn changes_inside(u: &Use, w: &Use) -> bool {
    let (held, written) = (u.place.proj.len(), w.place.proj.len());
    u.identity && w.access == Access::Unique && (written > held || (w.inside && written == held))
}

/// Whether Velt, as JavaScript, passes values of type `t` by reference: class instances and
/// arrays. An object type (`{ x: number }`) or a `struct` is inline: the call gets a copy made
/// when its argument is evaluated, so a later argument's change to it would not reach the
/// callee, as it does in JavaScript.
pub(super) fn by_reference(cx: &Ctx, t: crate::hir::TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => cx
            .adt(*d)
            .is_some_and(|a| a.kind == crate::hir::AdtKind::Class),
        TyKind::Array(_) => true,
        _ => false,
    }
}

/// Do two argument uses (`u` evaluated before `w`) conflict? Two moves, or a move followed
/// by a use, are left to the move analysis ("use of moved value").
fn clash(u: &Use, w: &Use) -> bool {
    if u.access == Access::Move || !u.place.overlaps(&w.place) {
        return false;
    }
    u.access == Access::Unique || w.access != Access::Shared
}

/// The mutable borrow is reported as the one the call holds, the other use as the error.
fn report_direct(u: &Use, w: &Use) -> Diagnostic {
    if w.access == Access::Unique && u.access != Access::Unique {
        report(w, u)
    } else {
        report(u, w)
    }
}

/// `held` is what the call keeps borrowed (label), `other` the conflicting use (primary span).
fn report(held: &Use, other: &Use) -> Diagnostic {
    let it = if held.text == other.text {
        "it".to_string()
    } else {
        shown(&held.text)
    };
    let name = shown(&other.text);
    let msg = match (held.access, other.access) {
        (Access::Unique, _) => {
            format!("cannot use {name} here: this call may modify {it} through another argument")
        }
        (_, Access::Move) => {
            format!("cannot move {name} here: {it} is already borrowed by this call")
        }
        _ => format!("cannot modify {name} here: {it} is already borrowed by this call"),
    };
    let label = match (held.access, held.captured) {
        (Access::Unique, true) => format!("{} is modified by this closure", shown(&held.text)),
        (Access::Unique, false) => "may be modified through this argument".to_string(),
        (_, true) => format!("{} is captured by this closure", shown(&held.text)),
        (_, false) => "borrowed here".to_string(),
    };
    Diagnostic::error(msg, other.span)
        .with_label(held.span, label)
        .with_note(
            "a value the callee may modify must not be reachable through another argument \
             of the same call; use a separate variable or `.clone()`",
        )
}
