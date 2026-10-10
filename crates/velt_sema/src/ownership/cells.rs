//! Shared cells for captured variables (semantics stage 2, docs/design/semantics-stage2.md §5).
//! An escaping closure captures its variables by value; when it or the enclosing function
//! assigns such a variable while the other still sees it (found by `crate::moves`), the variable
//! instead lives in a counted cell that the function and every closure capturing it share, so
//! `let count = 0; const inc = () => count++; inc(); console.log(count)` prints 1, as in JS.
//!
//! This pass marks those locals `LocalDef::boxed` — in the enclosing function and in each
//! closure capturing them (transitively) — and turns moves out of them into shares or copies:
//! a cell is never moved from, since a closure may still read it.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::body::places::{place_root, set_place_mode};
use crate::ctx::Ctx;
use crate::hir::{Def, DefId, Expr, ExprKind as E, LocalId, UseMode};
use crate::visit;

use super::soft::{is_moved_place, make_share};

/// Mark the variables of `boxed` (per function) and the captures of them as cells.
///
/// A cell is one variable shared by every function that sees it, so marking a closure's
/// capture local also marks the variable it captured in the enclosing function (#904: a
/// closure created in a closure assigns a variable the outer one captured), and from there the
/// captures of it in every other closure. `per_iteration` are the `for (let …)` variables the
/// loop's update assigns (`crate::moves::Outcome::per_iteration`): each iteration has its own,
/// so none of them becomes one cell from below.
pub(crate) fn box_cells(
    cx: &mut Ctx,
    boxed: &HashMap<DefId, HashSet<LocalId>>,
    per_iteration: &HashSet<(DefId, LocalId)>,
) {
    let mut work: Vec<(DefId, LocalId)> = boxed
        .iter()
        .flat_map(|(d, ls)| ls.iter().map(move |l| (*d, *l)))
        .collect();
    if work.is_empty() {
        return;
    }
    let parents = enclosing_fns(cx);
    let mut threaded = None;
    let mut done = HashSet::new();
    while let Some((d, l)) = work.pop() {
        if !done.insert((d, l)) {
            continue;
        }
        work.extend(box_local(cx, d, l));
        let Some(&p) = parents.get(&d) else { continue };
        let up: Vec<LocalId> = match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => f
                .captures
                .iter()
                .filter(|cap| cap.inner == l)
                .map(|cap| cap.outer)
                .collect(),
            _ => vec![],
        };
        for outer in up {
            if done.contains(&(p, outer)) {
                continue;
            }
            let threaded = threaded.get_or_insert_with(|| crate::moves::async_captured(cx));
            let why = if threaded.contains(&(p, outer)) {
                Some(Unshareable::Threaded)
            } else if !boxable_local(cx, p, outer) {
                Some(Unshareable::Promise)
            } else if per_iteration.contains(&(p, outer)) {
                Some(Unshareable::PerIteration)
            } else {
                None
            };
            let Some(why) = why else {
                work.push((p, outer));
                continue;
            };
            done.insert((p, outer));
            if !(why == Unshareable::Threaded && reported_threaded(cx, &parents, p, outer)) {
                unshareable(cx, (d, l), (p, outer), why);
            }
        }
    }
}

/// The headline of the error for a variable that a closure created inside another closure
/// assigns and that cannot live in a cell, after the variable's name.
pub(crate) const NESTED_ASSIGN: &str =
    "is assigned by a closure created inside another closure, and cannot be shared with it";

/// Why a variable a nested closure assigns cannot live in a cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unshareable {
    /// An async closure that may run on another thread captures it.
    Threaded,
    /// It holds a promise (`crate::moves`: not boxable).
    Promise,
    /// A `for (let …)` variable the loop's update assigns: one per iteration.
    PerIteration,
}

/// The note explaining `why` variable `name` cannot be shared.
pub(crate) fn unshareable_note(name: &str, why: Unshareable) -> String {
    match why {
        Unshareable::Threaded => format!(
            "a closure that runs on several threads (an HTTP handler, a spawned async closure) captures `{name}` too, and each of its runs would see its own copy; share it with `shared(...)`"
        ),
        Unshareable::Promise => format!(
            "`{name}` holds a promise, which cannot be shared; assign it in the function that declares it"
        ),
        Unshareable::PerIteration => format!(
            "each iteration of the `for` loop has its own `{name}`, which the loop's update copies into the next one; assign a variable declared in the loop body instead"
        ),
    }
}

/// May local `l` of function `d` live in a cell (`crate::moves`: not a promise)?
fn boxable_local(cx: &mut Ctx, d: DefId, l: LocalId) -> bool {
    let ty = match &cx.defs[d.0 as usize] {
        Some(Def::Fn(f)) => f.body.locals[l.0 as usize].ty,
        _ => return false,
    };
    cx.is_copy(ty) || cx.is_shared_value(ty)
}

/// Was a closure created in `p` that captures `outer` already reported for modifying it
/// (`crate::ownership::local_async`, which counts the closures created inside it)? Then the
/// threaded case says nothing more.
fn reported_threaded(cx: &Ctx, parents: &HashMap<DefId, DefId>, p: DefId, outer: LocalId) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[p.0 as usize] else {
        return false;
    };
    let name = &f.body.locals[outer.0 as usize].name;
    parents.iter().filter(|(_, q)| **q == p).any(|(c, _)| {
        let captures = match &cx.defs[c.0 as usize] {
            Some(Def::Fn(cf)) => cf.captures.iter().any(|cap| cap.outer == outer),
            _ => false,
        };
        let span = cx.def_spans.get(c.0 as usize);
        captures
            && cx
                .reported_captures
                .iter()
                .any(|(n, s)| n == name && Some(s) == span)
    })
}

/// A closure created in a closure assigns variable `outer` of `p` (through `inner`, the capture
/// of it by closure `d`), which cannot live in a cell (`why`).
fn unshareable(
    cx: &mut Ctx,
    (d, inner): (DefId, LocalId),
    (p, outer): (DefId, LocalId),
    why: Unshareable,
) {
    let Some(Def::Fn(f)) = &cx.defs[p.0 as usize] else {
        return;
    };
    let local = &f.body.locals[outer.0 as usize];
    let name = local.name.clone();
    let declared = local.span;
    let at = assignment_in(cx, d, inner)
        .or_else(|| cx.def_spans.get(d.0 as usize).copied())
        .unwrap_or(declared);
    cx.error(
        Diagnostic::error(format!("`{name}` {NESTED_ASSIGN}"), at)
            .with_label(declared, format!("`{name}` is declared here"))
            .with_note(unshareable_note(&name, why)),
    );
}

/// Where function `d` assigns its local `l` first, itself or else through a closure created
/// in it that captures `l`.
fn assignment_in(cx: &mut Ctx, d: DefId, l: LocalId) -> Option<Span> {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return None;
    };
    let mut found: Option<Span> = None;
    let mut closures = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
        E::Assign { place, .. } | E::CompoundAssign { place, .. } if matches!(place.kind, E::Local(x, _) if x == l) => {
            if found.is_none_or(|s| e.span.lo < s.lo) {
                found = Some(e.span);
            }
        }
        E::Closure(c) => closures.push(*c),
        _ => {}
    });
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    if found.is_some() {
        return found;
    }
    let inner: Vec<(DefId, LocalId)> = closures
        .into_iter()
        .filter_map(|c| match &cx.defs[c.0 as usize] {
            Some(Def::Fn(cf)) => cf
                .captures
                .iter()
                .find(|cap| cap.outer == l)
                .map(|cap| (c, cap.inner)),
            _ => None,
        })
        .collect();
    inner.into_iter().find_map(|(c, i)| assignment_in(cx, c, i))
}

/// Per closure: the function that creates it.
fn enclosing_fns(cx: &mut Ctx) -> HashMap<DefId, DefId> {
    let mut out = HashMap::new();
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Closure(c) = e.kind {
                out.insert(c, DefId(i as u32));
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
    }
    out
}

/// Make local `l` of function `d` a cell; returns the capture locals of the closures created in
/// `d` that capture it.
fn box_local(cx: &mut Ctx, d: DefId, l: LocalId) -> Vec<(DefId, LocalId)> {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return vec![];
    };
    f.body.locals[l.0 as usize].boxed = true;
    let mut closures = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Closure(c) = e.kind {
            closures.push(c);
        } else if is_moved_place(e) && place_root(e) == Some(l) {
            if cx.is_copy(e.ty) {
                set_place_mode(e, UseMode::Copy);
            } else {
                make_share(e);
            }
        }
    });
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    let mut inner = vec![];
    for c in closures {
        if let Some(Def::Fn(cf)) = &cx.defs[c.0 as usize] {
            inner.extend(
                cf.captures
                    .iter()
                    .filter(|cap| cap.outer == l)
                    .map(|cap| (c, cap.inner)),
            );
        }
    }
    inner
}
