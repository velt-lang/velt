//! `for...of` over a place (not a consumed temporary): the loop reads the array's length once,
//! re-reads its buffer on every iteration and binds each element by reference, so the body
//! must not modify or move the array itself or anything that owns it (`xs.push(..)`,
//! `xs = [..]`, `this.reset()` while iterating `this.items`): the binding would point into a
//! freed buffer, and the loop past its end. Changing elements in place (`x.done = true`,
//! `xs[i].push(..)`) is fine.
//!
//! Moving the array (or what owns it) out inside the body is the exception when it is a soft
//! move of a shared value (`super::super::shares`): it becomes a share instead, as JS hands the
//! callee the same array (`encode(h.f(v), handlers)` inside `for (const h of handlers)`). The
//! share makes the array type counted, and a loop over a counted array holds its own reference,
//! re-reads the length every iteration and shares each element into the binding
//! (docs/internals/design/semantics-stage2.md §3.3), so whatever the new owner does with it
//! (push, clear, drop) cannot free the buffer or element the loop is looking at.

use rustc_hash::FxHashSet;

use velt_common::{Diagnostic, Span};

use crate::hir::{Block, Expr};
use crate::visit;

use super::uses::{Access, Collector, Place, Proj};

/// Is `outer` the place `inner` or a place that contains it (so modifying `outer` may
/// reallocate or free `inner`)?
pub(super) fn contains(outer: &Place, inner: &Place) -> bool {
    outer.root == inner.root
        && outer.proj.len() <= inner.proj.len()
        && outer.proj.iter().zip(&inner.proj).all(|pair| match pair {
            (Proj::Field(a), Proj::Field(b)) => a == b,
            _ => true,
        })
}

/// Turn the moves of the iterated place `iter` (or of a place containing it) inside the loop
/// body into shares, where they are soft moves of a shared value (`soft`: the function's
/// `FnInfo::soft_moves`; a `using` variable or a disposed value is never among them).
pub(super) fn share_moved_iterated(
    col: &Collector,
    iter: &Place,
    soft: &FxHashSet<Span>,
    body: &mut Block,
) {
    let mut uses = vec![];
    col.nested_block(body, &mut uses);
    let spans: FxHashSet<Span> = uses
        .into_iter()
        .filter(|u| {
            u.access == Access::Move
                && !u.captured
                && soft.contains(&u.span)
                && contains(&u.place, iter)
        })
        .map(|u| u.span)
        .collect();
    if spans.is_empty() {
        return;
    }
    visit::exprs_mut(body, &mut |e: &mut Expr| {
        if spans.contains(&e.span) && super::super::soft::is_moved_place(e) {
            super::super::soft::make_share(e);
        }
    });
}

/// The first modification of the iterated place `iter` (named `text`, iterated at `at`)
/// inside the loop body.
pub(super) fn modified_while_iterating(
    col: &Collector,
    iter: &Place,
    text: &str,
    at: Span,
    body: &mut Block,
) -> Option<Diagnostic> {
    let mut uses = vec![];
    col.nested_block(body, &mut uses);
    let hit = uses
        .into_iter()
        .find(|u| u.access != Access::Shared && contains(&u.place, iter))?;
    let verb = match hit.access {
        Access::Move => "move",
        _ => "modify",
    };
    Some(
        Diagnostic::error(
            format!(
                "cannot {verb} `{}` while a `for...of` loop iterates over `{text}`",
                hit.text
            ),
            hit.span,
        )
        .with_label(at, "the loop borrows the array here")
        .with_note(format!(
            "the loop points into `{text}` until it ends; collect the changes and apply them after the loop, or loop over indexes (`for (let i = 0; i < {text}.length; i++)`)"
        )),
    )
}
