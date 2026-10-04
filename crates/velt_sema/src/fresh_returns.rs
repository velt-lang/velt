//! Which call results are fresh (#268): a `C[]` from `mk()` widens to `Named[]` by building a
//! new array (`body::expr::widen_fresh`), which is only what TypeScript does when nothing else
//! references the value. Velt's values are shared like JavaScript's, so `get items() { return
//! this.cs; }` returns the object's own array: converting a copy would make writes through the
//! `Named[]` miss it. The widening is checked when it is made, and its callees here, once every
//! body is checked: a function returns fresh values when each `return` is a literal, `new`, a
//! call of a function that returns fresh values (recursion assumes it does: a greatest fixpoint),
//! or a local initialized and assigned only with such values and otherwise only read, indexed or
//! used as the receiver of a builtin array operation (`out.push(x)`), never stored, passed on or
//! captured. Anything else is reported at the widening, with the conversion to write instead.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, Stmt, StmtKind as S, TyId, UseMode,
};
use crate::visit::{self, VisitMut};

/// A widened call result, valid if every function in `callees` returns fresh values.
pub(crate) struct FreshCheck {
    pub callees: Vec<DefId>,
    pub span: Span,
    pub from: TyId,
    pub to: TyId,
}

/// The functions a value's freshness depends on, if it is fresh when they return fresh values:
/// a literal, `new` or a builtin copy (none), a direct call (its callee), `await` of one.
pub(crate) fn fresh_callees(h: &Expr) -> Option<Vec<DefId>> {
    match &h.kind {
        E::Call { callee, .. } => match callee {
            Callee::Def(d, _) => Some(vec![*d]),
            Callee::Intrinsic(i) => fresh_intrinsic(*i).then(Vec::new),
            _ => None,
        },
        E::New { .. } | E::ArrayLit(_) | E::AdtLit { .. } => Some(vec![]),
        E::Await(x) => fresh_callees(x),
        E::Block(b) => b.value.as_deref().and_then(fresh_callees),
        _ => None,
    }
}

/// Builtins whose result is a new value.
fn fresh_intrinsic(i: Intrinsic) -> bool {
    matches!(i, Intrinsic::ArrayWithCapacity | Intrinsic::Clone)
}

/// Report the widened call results whose callees may return a value something else references.
pub(crate) fn check(cx: &mut Ctx) {
    let checks = std::mem::take(&mut cx.fresh_checks);
    if checks.is_empty() {
        return;
    }
    let fresh = fresh_functions(cx, checks.iter().flat_map(|c| c.callees.iter().copied()));
    for c in checks {
        if let Some(d) = c.callees.iter().find(|d| !fresh.contains(d)) {
            report(cx, *d, &c);
        }
    }
}

fn report(cx: &mut Ctx, d: DefId, c: &FreshCheck) {
    let name = cx.fn_info(d).name.clone();
    let name = name.rsplit("::").next().unwrap_or(&name).to_string();
    let (from, to) = (cx.display(c.from), cx.display(c.to));
    let fix = match cx.ty.array_elem(c.to) {
        Some(el) => format!(
            "convert a copy instead: `(…).map((x): {} => x)` (a new `{to}`)",
            cx.display(el)
        ),
        None => format!("create a new `{to}` from its fields instead"),
    };
    cx.error(
        Diagnostic::error(
            format!("cannot use the `{from}` that `{name}` returns as `{to}`"),
            c.span,
        )
        .with_note(format!(
            "TypeScript allows this, but `{name}` may return a value that other code still references as `{from}` (a field, a parameter, a stored value); Velt would convert a copy, so writes through the `{to}` would not reach it"
        ))
        .with_note(fix),
    );
}

/// The functions among `roots` (and the functions their results depend on) that return fresh
/// values: start from those whose own `return`s qualify, then drop any that depends on one that
/// does not, until nothing changes.
fn fresh_functions(cx: &Ctx, roots: impl Iterator<Item = DefId>) -> HashSet<DefId> {
    let mut deps: HashMap<DefId, Option<Vec<DefId>>> = HashMap::new();
    let mut work: Vec<DefId> = roots.collect();
    while let Some(d) = work.pop() {
        if deps.contains_key(&d) {
            continue;
        }
        let own = own_fresh(cx, d);
        if let Some(ds) = &own {
            work.extend(ds.iter().copied());
        }
        deps.insert(d, own);
    }
    let mut fresh: HashSet<DefId> = deps
        .iter()
        .filter(|(_, v)| v.is_some())
        .map(|(d, _)| *d)
        .collect();
    loop {
        let dropped: Vec<DefId> = fresh
            .iter()
            .copied()
            .filter(|d| {
                let ds = deps[d].as_deref().unwrap_or_default();
                ds.iter().any(|x| !fresh.contains(x))
            })
            .collect();
        if dropped.is_empty() {
            return fresh;
        }
        for d in dropped {
            fresh.remove(&d);
        }
    }
}

/// `Some(callees)` when every value function `d` returns is fresh provided `callees` return
/// fresh values.
fn own_fresh(cx: &Ctx, d: DefId) -> Option<Vec<DefId>> {
    let Some(Some(Def::Fn(f))) = cx.defs.get(d.0 as usize) else {
        return None;
    };
    if f.is_generator {
        return None;
    }
    let mut body = f.body.block.clone();
    let mut uses = Uses::default();
    visit::block(&mut body, &mut uses);
    if let Some(v) = &body.value {
        uses.returns.push((**v).clone());
    }
    let mut r = Returns {
        callees: vec![],
        returned: HashMap::new(),
    };
    let returns = std::mem::take(&mut uses.returns);
    if !returns.iter().all(|e| r.expr(e, true)) {
        return None;
    }
    let params: HashSet<LocalId> = f.params.iter().map(|p| p.local).collect();
    let locals: Vec<LocalId> = r.returned.keys().copied().collect();
    for l in locals {
        if !fresh_local(cx, l, &params, &uses, &mut r) {
            return None;
        }
    }
    Some(r.callees)
}

/// Local `l`, returned `r.returned[l]` times, holds only fresh values nothing else references.
fn fresh_local(
    cx: &Ctx,
    l: LocalId,
    params: &HashSet<LocalId>,
    uses: &Uses,
    r: &mut Returns,
) -> bool {
    if params.contains(&l) || captured(cx, l, uses) {
        return false;
    }
    let Some(inits) = uses.inits.get(&l) else {
        return false;
    };
    let total = uses.total.get(&l).copied().unwrap_or(0);
    let allowed = uses.allowed.get(&l).copied().unwrap_or(0);
    if total != allowed + r.returned[&l] {
        return false;
    }
    inits.iter().all(|e| r.expr(e, false))
}

/// Does a closure created in the body capture `l`?
fn captured(cx: &Ctx, l: LocalId, uses: &Uses) -> bool {
    uses.closures
        .iter()
        .any(|c| match cx.defs.get(c.0 as usize) {
            Some(Some(Def::Fn(cf))) => cf.captures.iter().any(|cap| cap.outer == l),
            _ => true,
        })
}

/// The returned values of one function, checked for freshness.
struct Returns {
    /// Functions whose results are returned.
    callees: Vec<DefId>,
    /// How often each local is returned.
    returned: HashMap<LocalId, u32>,
}

impl Returns {
    /// Is `e` fresh (a returned local only provisionally, see `fresh_local`)? A local is fresh
    /// only where it is returned (`ret`), not where it initializes another.
    fn expr(&mut self, e: &Expr, ret: bool) -> bool {
        match &e.kind {
            E::Local(l, _) if ret => {
                *self.returned.entry(*l).or_insert(0) += 1;
                true
            }
            E::If { then, els, .. } => self.expr(then, ret) && self.expr(els, ret),
            E::Match { arms, .. } => arms.iter().all(|a| self.expr(&a.body, ret)),
            E::Block(b) => b.value.as_deref().is_some_and(|v| self.expr(v, ret)),
            E::Await(x) => self.expr(x, ret),
            _ => match fresh_callees(e) {
                Some(ds) => {
                    self.callees.extend(ds);
                    true
                }
                None => false,
            },
        }
    }
}

/// How the locals of a body are used.
#[derive(Default)]
struct Uses {
    /// `return` values.
    returns: Vec<Expr>,
    /// Every value a local is initialized or assigned with.
    inits: HashMap<LocalId, Vec<Expr>>,
    /// Occurrences of each local.
    total: HashMap<LocalId, u32>,
    /// Occurrences that neither store nor pass on its value: read through (`xs.length`,
    /// `xs[i]`, a field), the receiver of a builtin (`xs.push(x)`), assigned to.
    allowed: HashMap<LocalId, u32>,
    /// Closures created in the body.
    closures: Vec<DefId>,
}

impl Uses {
    fn allow(&mut self, e: &Expr) {
        if let E::Local(l, m) = &e.kind {
            if matches!(m, UseMode::Borrow | UseMode::BorrowMut | UseMode::Copy) {
                *self.allowed.entry(*l).or_insert(0) += 1;
            }
        }
    }
}

impl VisitMut for Uses {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Return(Some(e)) => self.returns.push(e.clone()),
            S::Let {
                local,
                init: Some(e),
            } => self.inits.entry(*local).or_default().push(e.clone()),
            S::Let { local, init: None } => {
                self.inits.entry(*local).or_default();
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Local(l, _) => *self.total.entry(*l).or_insert(0) += 1,
            E::Closure(c) => self.closures.push(*c),
            E::Field { base, .. } | E::Index { base, .. } => self.allow(base),
            E::Call {
                callee: Callee::Intrinsic(i),
                args,
            } if !matches!(i, Intrinsic::Share | Intrinsic::Transfer) => {
                if let Some(a) = args.first() {
                    self.allow(a);
                }
            }
            E::Assign { place, value } => {
                if let E::Local(l, _) = place.kind {
                    *self.allowed.entry(l).or_insert(0) += 1;
                    self.inits.entry(l).or_default().push((**value).clone());
                }
            }
            _ => {}
        }
    }
}
