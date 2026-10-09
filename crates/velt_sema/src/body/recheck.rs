//! Checking a function body again: what a check adds to the context, and how to undo it.
//!
//! A body is checked again when its first check learned something the check itself needed:
//! the inferred return type of a recursive function (`recursion`), or the integer type of a
//! local declared from a literal (`literal_locals`). Before that, what the first check added
//! (diagnostics, deferred checks, closures, IDE records) is dropped:
//! - after a recursive function's first pass everything is dropped, and the bodies checked
//!   meanwhile are checked again when next needed (their types may depend on the placeholder);
//! - after a literal-local pass only the body's own additions are dropped. Bodies it checked
//!   meanwhile (`ensure_body` of a callee) don't depend on its locals, so they keep their
//!   results: each one's additions form a contiguous segment, recorded in its [`Frame`]. The
//!   closures the first check created are created again, in the same order, by the next one,
//!   which reuses their definitions (and names), so none is left behind.

use crate::ctx::Ctx;
use crate::defs::{BodyState, RetSource};
use crate::hir::{Def, DefId};

/// Lengths of the lists that checking a body appends to.
#[derive(Clone, Copy)]
pub(crate) struct Lens {
    diags: usize,
    throw_checks: usize,
    fresh_checks: usize,
    object_copies: [usize; 3],
    jsx_adapters: usize,
    fn_values: usize,
    fn_defs: usize,
    closures: usize,
    reported: usize,
    completed: usize,
    ide: Option<[usize; 4]>,
}

impl Lens {
    pub(crate) fn now(cx: &Ctx) -> Self {
        Lens {
            diags: cx.diags.len(),
            throw_checks: cx.throw_checks.len(),
            fresh_checks: cx.fresh_checks.len(),
            object_copies: cx.object_copies.mark(),
            jsx_adapters: cx.jsx_adapters.len(),
            fn_values: cx.fn_values.len(),
            fn_defs: cx.fn_defs.len(),
            closures: cx.closure_defs.len(),
            reported: cx.rec.reported.len(),
            completed: cx.rec.completed.len(),
            ide: cx
                .ide
                .as_ref()
                .map(|r| [r.refs.len(), r.types.len(), r.scopes.len(), r.params.len()]),
        }
    }

    /// The diagnostics' length.
    pub(crate) fn diags(&self) -> usize {
        self.diags
    }
}

/// One body being checked (`Ctx::rechecks`, innermost last).
#[derive(Default)]
pub(crate) struct Frame {
    /// What the bodies it checked meanwhile added: (start, end) of each.
    segments: Vec<(Lens, Lens)>,
    /// Closures of a rolled-back check, for the next check to create again (last first).
    reuse: Vec<DefId>,
}

/// The start of a body's check, to roll back to.
pub(crate) struct Mark {
    lens: Lens,
    /// The body's function and how many of its closures were numbered then (`None`: a mark
    /// inside a body, which only `rollback` uses).
    closures: Option<(String, Option<u32>)>,
    /// How many segments the frame that `rollback` finds had then: 0 for a body's start (its
    /// frame is entered after the mark), the current frame's count for a mark inside a body.
    segments: usize,
}

impl Frame {
    /// Drops the segments recorded after the first `n`: bodies checked since a mark, which its
    /// rollback has undone. (A segment's end can't tell: it is read before the body itself is
    /// recorded as completed, so a body that checked nothing ends where the mark started.)
    fn truncate_segments(&mut self, n: usize) {
        self.segments.truncate(n);
    }
}

/// Keep, of `v`'s elements from `from` on, those inside `keep` (index ranges), in order; returns
/// the others.
fn keep_ranges<T>(v: &mut Vec<T>, from: usize, keep: &[(usize, usize)]) -> Vec<T> {
    let tail = v.split_off(from.min(v.len()));
    let mut dropped = vec![];
    for (i, x) in tail.into_iter().enumerate() {
        if keep.iter().any(|&(s, e)| (s..e).contains(&(from + i))) {
            v.push(x);
        } else {
            dropped.push(x);
        }
    }
    dropped
}

impl Mark {
    /// The lists as they are now, before checking `def`'s body.
    pub(crate) fn new(cx: &Ctx, def: DefId) -> Self {
        let name = cx.fn_info(def).name.clone();
        let count = cx.closure_counts.get(&name).copied();
        Mark {
            lens: Lens::now(cx),
            closures: Some((name, count)),
            segments: 0,
        }
    }

    /// The lists as they are now, inside the body being checked: for a try that may be undone
    /// with `rollback` (`expr/jsx/list_fold.rs`).
    pub(crate) fn here(cx: &Ctx) -> Self {
        Mark {
            lens: Lens::now(cx),
            closures: None,
            segments: cx.rechecks.last().map_or(0, |f| f.segments.len()),
        }
    }

    pub(crate) fn lens(&self) -> Lens {
        self.lens
    }

    /// Undoes everything checked since the mark: what was added is dropped (closures created
    /// meanwhile are left unreferenced, without a body), and the bodies checked meanwhile are
    /// checked again when next needed (keeping the return types they inferred unless those
    /// depended on a placeholder).
    pub(crate) fn rollback(&self, cx: &mut Ctx) {
        let m = &self.lens;
        cx.diags.truncate(m.diags);
        cx.throw_checks.truncate(m.throw_checks);
        cx.fresh_checks.truncate(m.fresh_checks);
        cx.object_copies.rollback(m.object_copies);
        cx.jsx_adapters.truncate(m.jsx_adapters);
        cx.fn_values.truncate(m.fn_values);
        for d in cx.fn_defs.split_off(m.fn_defs) {
            drop_orphan(cx, d);
        }
        for c in cx
            .closure_defs
            .split_off(m.closures.min(cx.closure_defs.len()))
        {
            cx.callback_wrappers.remove(&c);
        }
        cx.rec.reported.truncate(m.reported);
        if let (Some(r), Some([refs, types, scopes, params])) = (&mut cx.ide, m.ide) {
            r.refs.truncate(refs);
            r.types.truncate(types);
            r.scopes.truncate(scopes);
            r.params.truncate(params);
        }
        for (g, inferred) in cx.rec.completed.split_off(m.completed) {
            cx.defs[g.0 as usize] = None;
            let error = cx.ty.has_error(cx.fn_info(g).ret);
            let f = cx.fn_info_mut(g);
            f.state = BodyState::Unchecked;
            if inferred && error {
                f.ret_source = RetSource::Body;
            }
        }
        // Bodies checked since the mark are unchecked again; those checked before it keep
        // their segments.
        if let Some(frame) = cx.rechecks.last_mut() {
            frame.truncate_segments(self.segments);
        }
    }

    /// Undoes what the body's own check added since the mark, keeping what the bodies it
    /// checked meanwhile added (their segments). Its closures are created again by the next
    /// check (`reuse_closure`).
    pub(crate) fn rollback_own(&self, cx: &mut Ctx) {
        let m = self.lens;
        let segments = cx
            .rechecks
            .last()
            .map(|f| f.segments.clone())
            .unwrap_or_default();
        let ranges = |f: fn(&Lens) -> usize| -> Vec<(usize, usize)> {
            segments.iter().map(|(s, e)| (f(s), f(e))).collect()
        };
        keep_ranges(&mut cx.diags, m.diags, &ranges(|l| l.diags));
        keep_ranges(
            &mut cx.throw_checks,
            m.throw_checks,
            &ranges(|l| l.throw_checks),
        );
        keep_ranges(
            &mut cx.fresh_checks,
            m.fresh_checks,
            &ranges(|l| l.fresh_checks),
        );
        let copies = &mut cx.object_copies;
        let start = m.object_copies;
        keep_ranges(
            &mut copies.copies,
            start[0],
            &ranges(|l| l.object_copies[0]),
        );
        keep_ranges(
            &mut copies.writes,
            start[1],
            &ranges(|l| l.object_copies[1]),
        );
        keep_ranges(&mut copies.seen, start[2], &ranges(|l| l.object_copies[2]));
        keep_ranges(
            &mut cx.jsx_adapters,
            m.jsx_adapters,
            &ranges(|l| l.jsx_adapters),
        );
        keep_ranges(&mut cx.fn_values, m.fn_values, &ranges(|l| l.fn_values));
        let own_fns = keep_ranges(&mut cx.fn_defs, m.fn_defs, &ranges(|l| l.fn_defs));
        let own_closures = keep_ranges(&mut cx.closure_defs, m.closures, &ranges(|l| l.closures));
        for c in &own_closures {
            cx.callback_wrappers.remove(c);
        }
        keep_ranges(&mut cx.rec.reported, m.reported, &ranges(|l| l.reported));
        keep_ranges(&mut cx.rec.completed, m.completed, &ranges(|l| l.completed));
        if let (Some(r), Some(start)) = (&mut cx.ide, m.ide) {
            let ide = |k: usize| -> Vec<(usize, usize)> {
                segments
                    .iter()
                    .filter_map(|(s, e)| Some((s.ide?[k], e.ide?[k])))
                    .collect()
            };
            keep_ranges(&mut r.refs, start[0], &ide(0));
            keep_ranges(&mut r.types, start[1], &ide(1));
            keep_ranges(&mut r.scopes, start[2], &ide(2));
            keep_ranges(&mut r.params, start[3], &ide(3));
        }
        for d in own_fns.iter().filter(|d| !own_closures.contains(d)) {
            drop_orphan(cx, *d);
        }
        let end = Lens::now(cx);
        if let Some(frame) = cx.rechecks.last_mut() {
            frame.segments = vec![(m, end)];
            frame.reuse = own_closures.into_iter().rev().collect();
        }
        if let Some((name, count)) = &self.closures {
            match count {
                Some(n) => cx.closure_counts.insert(name.clone(), *n),
                None => cx.closure_counts.remove(name),
            };
        }
    }
}

/// The function `d`, created by a check that was rolled back, is referenced by nothing: its
/// body and captures are emptied so that passes over every definition (move checking) find
/// nothing in it. (A function without a body would be an extern declaration.)
fn drop_orphan(cx: &mut Ctx, d: DefId) {
    if let Some(Def::Fn(f)) = &mut cx.defs[d.0 as usize] {
        f.body.block.stmts.clear();
        f.body.block.value = None;
        f.captures.clear();
    }
}

/// A closure definition of the rolled-back check of the body being checked, to create its
/// next closure in (`None`: allocate a new one).
pub(crate) fn reuse_closure(cx: &mut Ctx) -> Option<DefId> {
    cx.rechecks.last_mut()?.reuse.pop()
}

/// A body's check starts: the bodies it checks meanwhile record their segments for it.
pub(crate) fn enter(cx: &mut Ctx) {
    cx.rechecks.push(Frame::default());
}

/// A body's check that started at `start` is done: it is one segment of the body checking it.
/// Closures of an earlier check that this one did not create again stay unreferenced.
pub(crate) fn leave(cx: &mut Ctx, start: Lens) {
    let frame = cx.rechecks.pop().unwrap_or_default();
    for d in frame.reuse {
        drop_orphan(cx, d);
    }
    let end = Lens::now(cx);
    if let Some(outer) = cx.rechecks.last_mut() {
        outer.segments.push((start, end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens(completed: usize) -> Lens {
        Lens {
            diags: 0,
            throw_checks: 0,
            fresh_checks: 0,
            object_copies: [0; 3],
            jsx_adapters: 0,
            fn_values: 0,
            fn_defs: 0,
            closures: 0,
            reported: 0,
            completed,
            ide: None,
        }
    }

    /// A body checked after a mark inside the body (`Mark::here`) that checks nothing else ends
    /// with as many completed bodies as the mark saw: its rollback still drops its segment, and
    /// keeps the one recorded before the mark.
    #[test]
    fn rollback_drops_the_segments_after_a_mark_inside_a_body() {
        let mut frame = Frame::default();
        frame.segments.push((lens(0), lens(0)));
        // Mark::here: one segment so far, one completed body (the first one's).
        let at = frame.segments.len();
        let mark = lens(1);
        // A body checked during the try: its segment ends before it is completed.
        frame.segments.push((lens(1), lens(1)));
        assert_eq!(frame.segments[1].1.completed, mark.completed);
        frame.truncate_segments(at);
        assert_eq!(frame.segments.len(), 1);
        assert_eq!(frame.segments[0].1.completed, 0);
    }
}
