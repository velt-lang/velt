//! `for...of` over a place (not a consumed temporary): the loop reads the array's length once,
//! re-reads its buffer on every iteration and binds each element by reference, so the body
//! must not modify or move the array itself or anything that owns it (`xs.push(..)`,
//! `xs = [..]`, `this.reset()` while iterating `this.items`): the binding would point into a
//! freed buffer, and the loop past its end. Changing elements in place (`x.done = true`,
//! `xs[i].push(..)`) is fine. Replacing an element (`xs[i] = …`) is fine too, but it frees the
//! element a binding may refer to: such a loop's bindings take shares (`replaces_bound`).
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

use crate::hir::{Block, Expr, Pat, PatKind, UseMode};
use crate::visit;

use super::uses::{Access, Collector, Place, Proj, Use};

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

/// The places the loop body `body` mutates or moves.
pub(super) fn body_uses(col: &Collector, body: &mut Block) -> Vec<Use> {
    let mut uses = vec![];
    col.nested_block(body, &mut uses);
    uses
}

/// The first modification of the iterated place `iter` (named `text`, iterated at `at`)
/// among the loop body's `uses`.
pub(super) fn modified_while_iterating(
    uses: &[Use],
    iter: &Place,
    text: &str,
    at: Span,
) -> Option<Diagnostic> {
    let hit = uses
        .iter()
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

/// Does the loop body (its `uses`) replace, remove or move what a borrowing binding of
/// `binding` refers to in the elements of the iterated place `iter` (`xs[i] = …`,
/// `xs[i].f = …` under `const { f } of xs`, `row[0] = …` in a loop over `row`)? The binding
/// then takes a share of it (`StmtKind::ForOf::share_elems`), as JS keeps the old value alive.
/// Changes inside it (`v.n = 1`, `xs[i].n = 1` under `const v of xs`) leave it in place. The
/// binding's own uses are rooted at the binding, not at `iter`, while these are collected.
pub(super) fn replaces_bound(uses: &[Use], iter: &Place, binding: &Pat) -> bool {
    let mut elem = iter.clone();
    elem.proj.push(Proj::Index);
    let mut bound = vec![];
    bound_places(binding, elem, &mut bound);
    !bound.is_empty()
        && uses.iter().any(|u| {
            u.access != Access::Shared
                && bound.iter().any(|(b, exact)| match exact {
                    true => contains(&u.place, b),
                    false => u.place.overlaps(b),
                })
        })
}

/// The places the borrowing bindings of `pat` (matched against `place`) refer to: exact, or
/// (`false`) a place that holds what they refer to (a union's payload).
fn bound_places(pat: &Pat, place: Place, out: &mut Vec<(Place, bool)>) {
    let field = |i: usize| {
        let mut p = place.clone();
        p.proj.push(Proj::Field(i as u32));
        p
    };
    match &pat.kind {
        PatKind::Binding(_, UseMode::Borrow | UseMode::BorrowMut) => out.push((place, true)),
        PatKind::Tuple(ps) => {
            for (i, p) in ps.iter().enumerate() {
                bound_places(p, field(i), out);
            }
        }
        PatKind::Adt { fields } => {
            for (i, p) in fields {
                bound_places(p, field(*i as usize), out);
            }
        }
        PatKind::Array { elems, .. } => {
            for p in elems {
                let mut e = place.clone();
                e.proj.push(Proj::Index);
                bound_places(p, e, out);
            }
        }
        PatKind::Some(p) => bound_places(p, place, out),
        PatKind::Variant { args, .. } | PatKind::Or(args) => {
            let mut inner = vec![];
            for p in args {
                bound_places(p, place.clone(), &mut inner);
            }
            if !inner.is_empty() {
                out.push((place, false));
            }
        }
        PatKind::Binding(..)
        | PatKind::Wildcard
        | PatKind::Lit(_)
        | PatKind::None
        | PatKind::InstanceOf(_) => {}
    }
}
