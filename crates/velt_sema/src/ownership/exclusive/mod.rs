//! Exclusive access (docs/reference/memory.md "Exclusive access"): within one call, a place the callee
//! may modify is reachable through no other argument. Checked over the final HIR (after
//! ownership / mutation inference and the move analysis, so pass modes and clones are settled).
//!
//! Per call (`Call`, `New`), each argument's [`uses`] are compared pairwise:
//! - a place passed `BorrowMut` (an argument or receiver the callee is inferred to modify, a
//!   variable a closure argument modifies, the receiver of `Mutex.with`) must not overlap any
//!   place another argument borrows, mutably borrows, moves or captures;
//! - a place passed `Borrow` must not overlap a place another argument moves;
//! - no argument may mutate or move (while being evaluated) a place another argument passes by
//!   reference.
//!
//! Places overlap when one is a prefix of the other (`a` / `a.f`, `xs` / `xs[i]`; `xs[i]` and
//! `xs[j]` always overlap, `a.f` and `a.g` never do); borrowing pattern bindings count as
//! the place they borrow from. The non-Copy params of closures and of named functions used as
//! values may alias each other (calls through a function value don't check their arguments
//! against each other), so there they count as one place. Guarantees for code generation: during a call, memory reachable
//! from a `BorrowMut` parameter is not reachable from any other parameter (including the
//! captures of a closure argument), and `Borrow` parameters may alias each other but never a
//! `BorrowMut` or `Owned` one.

mod iteration;
mod let_borrow;
mod uses;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::defs::FnKind;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalDef, LocalId, Pat, PatKind,
    Stmt, StmtKind as S, UseMode,
};
use crate::ownership::validate::place_text;
use crate::visit::{self, VisitMut};

use uses::{Access, Aliases, Collector, Place, Proj, Use};

pub(crate) fn check_exclusive(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        // By-reference `const`s that the rest of their block would invalidate become shares
        // first, so the call checks below see what lowering will do.
        let mut errors = vec![];
        let shared: Vec<bool> = f
            .body
            .locals
            .clone()
            .iter()
            .map(|l| cx.is_shared_value(l.ty))
            .collect();
        let aliases = aliasing_params(cx, d, &f);
        let col = Collector {
            cx,
            locals: &f.body.locals,
            aliases: &aliases,
        };
        let_borrow::check_body(
            &col,
            &f.body.locals,
            &shared,
            &mut f.body.block,
            &mut errors,
        );
        let aliases = aliasing_params(cx, d, &f);
        let mut checker = Checker {
            cx,
            locals: &f.body.locals,
            aliases,
            errors: vec![],
        };
        visit::block(&mut f.body.block, &mut checker);
        errors.extend(checker.errors);
        cx.diags.extend(errors);
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
}

/// Closures and functions used as values: their non-Copy declared params may alias each other,
/// so they all stand for the first one.
fn aliasing_params(cx: &mut Ctx, d: DefId, f: &FnDef) -> Aliases {
    let mut out = Aliases::new();
    let info = cx.fn_info(d);
    let as_value = info.kind == FnKind::Closure || cx.fn_values.iter().any(|(v, _)| *v == d);
    if !as_value {
        return out;
    }
    let first = f.captures.len() + usize::from(f.self_ty.is_some());
    let mut shared: Option<LocalId> = None;
    for p in &f.params[first..] {
        if cx.is_copy(p.ty) {
            continue;
        }
        let root = *shared.get_or_insert(p.local);
        out.insert(p.local, Place { root, proj: vec![] });
    }
    out
}

struct Checker<'a, 'm> {
    cx: &'a Ctx<'m>,
    locals: &'a [LocalDef],
    aliases: Aliases,
    errors: Vec<Diagnostic>,
}

/// The argument uses of one call: `(direct, nested)` per argument.
type ArgUses = Vec<(Vec<Use>, Vec<Use>)>;

impl Checker<'_, '_> {
    fn collector(&self) -> Collector<'_, '_> {
        Collector {
            cx: self.cx,
            locals: self.locals,
            aliases: &self.aliases,
        }
    }

    /// Borrowing bindings of `pat` point into `place`.
    fn bind(&mut self, pat: &Pat, place: &Place) {
        match &pat.kind {
            PatKind::Binding(l, UseMode::Borrow | UseMode::BorrowMut) => {
                self.aliases.insert(*l, place.clone());
            }
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|p| self.bind(p, place))
            }
            PatKind::Array { elems, .. } => elems.iter().for_each(|p| self.bind(p, place)),
            PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| self.bind(p, place)),
            PatKind::Some(p) => self.bind(p, place),
            _ => {}
        }
    }

    fn check_call(&mut self, args: &mut [Expr], mutex_with: bool) {
        if args.len() < 2 {
            return;
        }
        let col = self.collector();
        let mut uses: ArgUses = vec![];
        for a in args.iter_mut() {
            let (mut direct, mut nested) = (vec![], vec![]);
            col.direct(a, &mut direct, &mut nested);
            uses.push((direct, nested));
        }
        // `m.with(f)`: the callback gets the lock, i.e. mutable access to `m`'s value.
        if mutex_with {
            for u in &mut uses[0].0 {
                u.access = Access::Unique;
            }
        }
        if let Some(d) = first_conflict(&uses) {
            self.errors.push(d);
        }
    }
}

fn first_conflict(uses: &ArgUses) -> Option<Diagnostic> {
    for (i, (direct_i, _)) in uses.iter().enumerate() {
        for (j, (direct_j, nested_j)) in uses.iter().enumerate() {
            if i == j {
                continue;
            }
            for u in direct_i {
                if i < j {
                    let hit = direct_j.iter().find(|w| clash(u, w));
                    if let Some(w) = hit {
                        return Some(report_direct(u, w));
                    }
                }
                let by_ref = matches!(u.access, Access::Shared | Access::Unique);
                let hit = nested_j
                    .iter()
                    .find(|w| by_ref && u.place.overlaps(&w.place));
                if let Some(w) = hit {
                    return Some(report(u, w));
                }
            }
        }
    }
    None
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
        format!("`{}`", held.text)
    };
    let name = &other.text;
    let msg = match (held.access, other.access) {
        (Access::Unique, _) => {
            format!("cannot use `{name}` here: this call may modify {it} through another argument")
        }
        (_, Access::Move) => {
            format!("cannot move `{name}` here: {it} is already borrowed by this call")
        }
        _ => format!("cannot modify `{name}` here: {it} is already borrowed by this call"),
    };
    let label = match (held.access, held.captured) {
        (Access::Unique, true) => format!("`{}` is modified by this closure", held.text),
        (Access::Unique, false) => "may be modified through this argument".to_string(),
        (_, true) => format!("`{}` is captured by this closure", held.text),
        (_, false) => "borrowed here".to_string(),
    };
    Diagnostic::error(msg, other.span)
        .with_label(held.span, label)
        .with_note(
            "a value the callee may modify must not be reachable through another argument \
             of the same call; use a separate variable or `.clone()`",
        )
}

impl VisitMut for Checker<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            S::ForOf {
                binding,
                iter,
                body,
                consume,
                ..
            } => {
                if let Some((mut p, _)) = self.collector().place_of(iter) {
                    if !*consume {
                        let text = place_text(self.cx, self.locals, iter);
                        let col = self.collector();
                        let hit =
                            iteration::modified_while_iterating(&col, &p, &text, iter.span, body);
                        self.errors.extend(hit);
                    }
                    p.proj.push(Proj::Index);
                    self.bind(binding, &p);
                }
            }
            S::LetPat { pat, init } => {
                if let Some((p, _)) = self.collector().place_of(init) {
                    self.bind(pat, &p);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            E::Match { scrutinee, arms } => {
                if let Some((p, _)) = self.collector().place_of(scrutinee) {
                    for a in arms.iter() {
                        self.bind(&a.pat, &p);
                    }
                }
            }
            E::Call { callee, args } => {
                let mutex_with = matches!(callee, Callee::Intrinsic(Intrinsic::MutexWith));
                self.check_call(args, mutex_with);
            }
            E::New { args, .. } => self.check_call(args, false),
            _ => {}
        }
    }
}
