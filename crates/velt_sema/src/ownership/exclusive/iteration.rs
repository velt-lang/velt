//! `for...of` over a place (not a consumed temporary): the loop reads the array's length once,
//! re-reads its buffer on every iteration and binds each element by reference, so the body
//! must not modify or move the array itself or anything that owns it (`xs.push(..)`,
//! `xs = [..]`, `this.reset()` while iterating `this.items`): the binding would point into a
//! freed buffer, and the loop past its end. Changing elements in place (`x.done = true`,
//! `xs[i].push(..)`) is fine.

use velt_common::{Diagnostic, Span};

use crate::hir::Block;

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
