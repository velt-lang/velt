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

use velt_common::Diagnostic;

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
/// captures of it in every other closure.
pub(crate) fn box_cells(cx: &mut Ctx, boxed: &HashMap<DefId, HashSet<LocalId>>) {
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
        let up: Vec<(DefId, LocalId)> = match &cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => f
                .captures
                .iter()
                .filter(|cap| cap.inner == l)
                .map(|cap| (p, cap.outer))
                .collect(),
            _ => vec![],
        };
        for (p, outer) in up {
            if done.contains(&(p, outer)) {
                continue;
            }
            let threaded = threaded.get_or_insert_with(|| crate::moves::async_captured(cx));
            let on_threads = threaded.contains(&(p, outer));
            if on_threads || !boxable_local(cx, p, outer) {
                done.insert((p, outer));
                unshareable(cx, p, outer, on_threads);
                continue;
            }
            work.push((p, outer));
        }
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

/// A closure created in a closure assigns variable `l` of `d`, which cannot live in a cell:
/// `threaded`, an async closure that may run on another thread captures it (else it holds a
/// promise).
fn unshareable(cx: &mut Ctx, d: DefId, l: LocalId, threaded: bool) {
    let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
        return;
    };
    let local = &f.body.locals[l.0 as usize];
    let name = local.name.clone();
    let span = local.span;
    let why = match threaded {
        true => format!("a closure that runs on several threads (an HTTP handler, a spawned async closure) captures `{name}` too, and each of its runs would see its own copy; share it with `shared(...)`"),
        false => format!("`{name}` holds a promise, which cannot be shared; assign it in the function that declares it"),
    };
    cx.error(
        Diagnostic::error(
            format!("`{name}` is assigned by a closure created inside another closure, and cannot be shared with it"),
            span,
        )
        .with_note(why),
    );
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
