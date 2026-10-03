//! Ownership and mutation inference (docs/reference/memory.md "Ownership",
//! "Mutation is inferred"), a fixpoint over all checked bodies:
//! - a non-Copy parameter (or `this`) that its body moves from becomes `Owned`, unless the
//!   function's ABI is fixed (closures, dynamically dispatched methods, externs);
//! - params, `this` and closure captures whose contents the body modifies become `BorrowMut`
//!   (`super::mutation`), and methods sharing a dispatch slot are joined;
//! - a `match` / destructuring binding into a place that is moved from becomes a `Move`
//!   binding, and the matched place is then moved (`UseMode::Move`);
//! - every call site follows the callee's modes (`super::patch`).
//!
//! Each step only turns borrows into mutable borrows or moves, so the iteration terminates
//! (at the least fixpoint, whatever the visiting order); `super::worklist` schedules it so
//! that only the bodies reading a changed function are visited again.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::body::places::{is_place, place_root, set_place_mode};
use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, PassMode, Pat,
    PatKind, Stmt, StmtKind, UseMode,
};
use crate::visit::{self, VisitMut};

pub(crate) fn infer_modes(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done && cx.defs[d.0 as usize].is_some())
        .collect();
    let dispatch = super::mutation::Dispatch::new(cx);
    let mut work = super::worklist::Worklist::new(cx, &fns, &dispatch);
    let position: HashMap<DefId, usize> = fns.iter().copied().zip(0..).collect();
    let mut reported = HashSet::new();
    // Errors found while patching, reported in definition order (not visiting order).
    let mut errors = vec![];
    let mut changed = vec![];
    // Bodies are revisited only after a real mode change, and modes only grow, so this ends
    // after a few visits per body; the cap only stops a runaway loop (an ICE).
    let (mut visits, cap) = (0, 64 * fns.len() + 10_000);
    while let Some(d) = work.next_body() {
        visits += 1;
        if visits > cap {
            // Reported in every build: bodies not visited again keep modes that are too weak,
            // which would surface as confusing errors in unrelated code.
            let info = cx.fn_info(d);
            let e = Diagnostic::error(
                format!(
                    "ICE: ownership inference does not settle (still changing `{}`)",
                    info.name
                ),
                info.name_span,
            )
            .with_note("this is a compiler bug; please report it with this program");
            errors.push((position[&d], e));
            break;
        }
        let before = cx.diags.len();
        let body_changed = with_body(cx, d, |cx, f| {
            let borrowed = super::fn_values::borrowed_fn_locals(cx, d, f);
            let patched =
                super::patch::patch_calls(cx, &mut f.body.block, &borrowed, &mut reported);
            infer_body(cx, d, f) | patched
        });
        errors.extend(cx.diags.drain(before..).map(|e| (position[&d], e)));
        if body_changed {
            work.changed(d);
        }
        while let Some(g) = work.next_join() {
            dispatch.join(cx, g, &mut changed);
            changed.drain(..).for_each(|m| work.changed(m));
        }
    }
    errors.sort_by_key(|(at, _)| *at);
    cx.diags.extend(errors.into_iter().map(|(_, e)| e));
    super::finish::demote_fn_values(cx);
    for &d in &fns {
        with_body(cx, d, |cx, f| {
            super::finish::reassigned_fixed_params(cx, d, f);
            super::finish::soft_args(cx, d, f);
            sync_params(cx, d, f);
            false
        });
    }
}

/// Run `op` on def `d`'s body (taken out of the context meanwhile).
pub(super) fn with_body(
    cx: &mut Ctx,
    d: DefId,
    op: impl FnOnce(&mut Ctx, &mut FnDef) -> bool,
) -> bool {
    let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
        return false;
    };
    let r = op(cx, &mut f);
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    r
}

/// Locals moved from (wholly or partially), incl. by escaping-closure captures. Soft moves
/// (async-call arguments, `soft`) do not count: they become clones rather than take ownership.
/// Neither do strings taken out of a larger value or captured by a closure: they become copies
/// when the root stays alive (`super::strings`), so `return this.name` borrows `this`; nor do
/// fields taken out of a class instance, which become shares.
fn moved_roots(cx: &Ctx, b: &mut Block, soft: &HashSet<Span>) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    visit::exprs_mut(b, &mut |e: &mut Expr| match &e.kind {
        // A copy of a function value outlives the call: a function-typed param that is copied
        // (`.clone()`, an async-call argument) must own its closure (`super::fn_values`).
        E::Local(l, UseMode::Move) if super::fn_values::is_fn(cx, e.ty) => {
            out.insert(*l);
        }
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Clone | Intrinsic::Share),
            args,
        } => {
            if let [a @ Expr {
                kind: E::Local(l, _),
                ..
            }] = args.as_slice()
            {
                if super::fn_values::is_fn(cx, a.ty) {
                    out.insert(*l);
                }
            }
        }
        _ if soft.contains(&e.span) && super::soft::is_moved_place(e) => {}
        E::Field { .. } | E::UnwrapSome(..) | E::UnwrapVariant { .. }
            if cx.is_string_value(e.ty) => {}
        // A field can't leave a class instance: the move becomes a share (`super::validate`),
        // so it takes nothing from the root (`get signal() { return this.ctl.sig; }` borrows
        // `this`).
        E::Field { .. } | E::UnwrapSome(..) | E::UnwrapVariant { .. } if through_class(cx, e) => {}
        E::Local(l, UseMode::Move) => {
            out.insert(*l);
        }
        E::Field {
            mode: UseMode::Move,
            ..
        }
        | E::UnwrapSome(_, UseMode::Move)
        | E::UnwrapVariant {
            mode: UseMode::Move,
            ..
        } => {
            if let Some(l) = place_root(e) {
                out.insert(l);
            }
        }
        E::Closure(def) => {
            if let Some(Def::Fn(f)) = &cx.defs[def.0 as usize] {
                let owned = f.captures.iter().filter(|c| c.mode == PassMode::Owned);
                for c in owned {
                    if !cx.is_string_value(f.body.locals[c.inner.0 as usize].ty) {
                        out.insert(c.outer);
                    }
                }
            }
        }
        _ => {}
    });
    out
}

/// Does place `e` lie inside a class instance (a field reached through one)?
fn through_class(cx: &Ctx, e: &Expr) -> bool {
    let mut cur = e;
    loop {
        cur = match &cur.kind {
            E::Field { base, .. }
            | E::UnwrapSome(base, _)
            | E::UnwrapVariant { expr: base, .. } => base,
            _ => return false,
        };
        if cx.class_of(cur.ty).is_some() {
            return true;
        }
    }
}

fn infer_body(cx: &mut Ctx, d: DefId, f: &mut FnDef) -> bool {
    let soft: HashSet<Span> = cx.fn_info(d).soft_moves.iter().copied().collect();
    let moved = moved_roots(cx, &mut f.body.block, &soft);
    let mut changed = false;
    let fixed = cx.fn_info(d).fixed_modes;
    let has_this = cx.fn_info(d).this.is_some();
    let first = f.captures.len();
    for (i, p) in f.params.iter().enumerate().skip(first) {
        let borrowed = matches!(p.mode, PassMode::Borrow | PassMode::BorrowMut);
        if fixed || !borrowed || !moved.contains(&p.local) {
            continue;
        }
        // The HIR params keep their checked modes until `sync_params`: only a change of the
        // inferred signature counts, or the fixpoint never settles.
        let info = cx.fn_info_mut(d);
        let slot = if has_this && i == 0 {
            info.this.as_mut().map(|t| &mut t.mode)
        } else {
            info.params
                .get_mut(i - usize::from(has_this))
                .map(|p| &mut p.mode)
        };
        if let Some(m) = slot.filter(|m| **m != PassMode::Owned) {
            *m = PassMode::Owned;
            changed = true;
        }
    }
    let ev = super::evidence::collect(cx, &mut f.body.block);
    changed |= super::mutation::apply(cx, d, f, &ev, &moved);
    let kinds = cx.fn_info(d).local_kinds.clone();
    let mut flip = Flip {
        moved: &moved,
        kinds: &kinds,
        changed: false,
    };
    visit::block(&mut f.body.block, &mut flip);
    changed | flip.changed
}

/// Turns borrowed bindings that are moved from into moves (and moves the matched place).
struct Flip<'a> {
    moved: &'a HashSet<LocalId>,
    kinds: &'a [LocalKind],
    changed: bool,
}

impl Flip<'_> {
    /// Returns whether the pattern has a `Move` binding afterwards.
    fn pat(&mut self, p: &mut Pat) -> bool {
        let mut any = false;
        let (moved, kinds) = (self.moved, self.kinds);
        let mut changed = false;
        visit::pat(
            p,
            &mut PatFn(&mut |x: &mut Pat| {
                if let PatKind::Binding(l, m) = &mut x.kind {
                    let bind = kinds.get(l.0 as usize) == Some(&LocalKind::Bind);
                    if *m == UseMode::Borrow && bind && moved.contains(l) {
                        *m = UseMode::Move;
                        changed = true;
                    }
                    any |= *m == UseMode::Move;
                }
            }),
        );
        self.changed |= changed;
        any
    }

    /// Move the matched place. Reports a change only when its mode really changes (an array
    /// element `xs[i]` included), or the fixpoint never settles; a module constant (no mode)
    /// is left alone.
    fn consume(&mut self, scrutinee: &mut Expr) {
        if current_mode(scrutinee).is_some_and(|m| m != UseMode::Move) {
            set_place_mode(scrutinee, UseMode::Move);
            self.changed = true;
        }
    }
}

struct PatFn<'a>(&'a mut dyn FnMut(&mut Pat));

impl VisitMut for PatFn<'_> {
    fn pat(&mut self, p: &mut Pat) {
        (self.0)(p);
    }
}

impl VisitMut for Flip<'_> {
    fn expr(&mut self, e: &mut Expr) {
        if let E::Match { scrutinee, arms } = &mut e.kind {
            let mut any = false;
            for a in arms.iter_mut() {
                any |= self.pat(&mut a.pat);
            }
            if any {
                self.consume(scrutinee);
            }
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        if let StmtKind::LetPat { pat, init } = &mut s.kind {
            if self.pat(pat) {
                self.consume(init);
            }
        }
    }
}

/// Consume place `e` (through `Upcast` / `WrapSome`); returns whether a mode changed.
pub(crate) fn force_move(cx: &mut Ctx, e: &mut Expr, errors: &mut Vec<Diagnostic>) -> bool {
    match &mut e.kind {
        E::Upcast(inner) | E::WrapSome(inner) => force_move(cx, inner, errors),
        E::Closure(def) => super::fn_values::escape_closure(cx, *def),
        E::Global(d) => {
            let (ty, name) = cx
                .global(*d)
                .map(|g| (g.ty, g.name.clone()))
                .unwrap_or((cx.ty.error, String::new()));
            if !cx.is_copy(ty) && ty != cx.ty.str_ {
                errors.push(
                    Diagnostic::error(
                        format!("cannot move out of module constant `{name}`"),
                        e.span,
                    )
                    .with_note(format!("use `{name}.clone()` for an owned copy")),
                );
            }
            false
        }
        _ => {
            if !is_place(e) {
                return false;
            }
            let m = if cx.is_copy(e.ty) {
                UseMode::Copy
            } else {
                UseMode::Move
            };
            if current_mode(e) == Some(m) {
                return false;
            }
            set_place_mode(e, m);
            true
        }
    }
}

fn current_mode(e: &Expr) -> Option<UseMode> {
    match e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => Some(m),
        _ => None,
    }
}

/// Copy the inferred modes into the HIR params (closure captures are updated in place).
fn sync_params(cx: &Ctx, d: DefId, f: &mut FnDef) {
    let info = cx.fn_info(d);
    let mut modes: Vec<PassMode> = info.this.iter().map(|t| t.mode).collect();
    modes.extend(info.params.iter().map(|p| p.mode));
    let skip = f.params.len().saturating_sub(modes.len());
    for (p, m) in f.params.iter_mut().skip(skip).zip(modes) {
        p.mode = m;
    }
}
