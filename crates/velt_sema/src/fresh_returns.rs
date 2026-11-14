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

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, Stmt, StmtKind as S, TyId,
    TyKind, UseMode,
};
use crate::visit::{self, VisitMut};

/// A widened call result, valid if every function in `callees` returns fresh values.
#[derive(Clone)]
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
        // A new object is fresh when its constructor keeps `this` to itself (`ctor_fresh`).
        E::New { def, .. } => Some(vec![*def]),
        E::ArrayLit(_) | E::ConstArray(_) | E::AdtLit { .. } => Some(vec![]),
        E::Await(x) => fresh_callees(x),
        // A conversion keeps the value: a new one converted to an interface or `any` is new.
        E::ToDyn { expr: x, .. } | E::Upcast(x) => fresh_callees(x),
        E::Block(b) => b.value.as_deref().and_then(fresh_callees),
        _ => None,
    }
}

/// Is `h` a value nothing else references yet: a literal, `new`, a builtin copy, or a call of a
/// function that returns fresh values (`slice()`, `map`, `concat`)? Only the value itself is
/// new: what it holds may be shared.
pub(crate) fn fresh_value(cx: &Ctx, h: &Expr) -> bool {
    let Some(cs) = fresh_callees(h) else {
        return false;
    };
    let fresh = fresh_functions(cx, cs.iter().copied());
    cs.iter().all(|c| fresh.contains(c))
}

/// Builtins whose result is a new value.
fn fresh_intrinsic(i: Intrinsic) -> bool {
    matches!(i, Intrinsic::ArrayWithCapacity | Intrinsic::Clone)
}

/// Report the widened call results whose callees may return a value something else references.
pub(crate) fn check(cx: &mut Ctx) {
    let mut checks = std::mem::take(&mut cx.fresh_checks);
    checks.append(&mut cx.object_impls.fresh_checks);
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
    if let Some(e) = crate::collect::c_sig::ptr_conversion(cx, c.from, c.to, c.span) {
        cx.error(e);
        return;
    }
    let name = match cx.adt(d) {
        Some(a) => format!("new {}", a.name),
        None => cx.fn_info(d).name.clone(),
    };
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
pub(crate) fn fresh_functions(cx: &Ctx, roots: impl Iterator<Item = DefId>) -> FxHashSet<DefId> {
    let mut deps: FxHashMap<DefId, Option<Vec<DefId>>> = FxHashMap::default();
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
    let mut fresh: FxHashSet<DefId> = deps
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
    if cx.adt(d).is_some() {
        return ctor_fresh(cx, d);
    }
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
        returned: FxHashMap::default(),
    };
    let returns = std::mem::take(&mut uses.returns);
    if !returns.iter().all(|e| r.expr(e, true)) {
        return None;
    }
    let params: FxHashSet<LocalId> = f.params.iter().map(|p| p.local).collect();
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
    params: &FxHashSet<LocalId>,
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

/// `Some(base classes)` when `new` of class `d` gives an object nothing else references,
/// provided the base classes' constructors do too: its constructor uses `this` only as the base
/// of field reads and writes, and as the receiver of `super(...)` (field initializers cannot use
/// `this`). Passing `this` on, storing it or capturing it lets it escape (`all.push(this)`). A call
/// of one of its own methods on `this` (`this.put(…)`) is allowed when every method the call may
/// run uses its `this` the same way ([`keeps_this`], #2893).
fn ctor_fresh(cx: &Ctx, d: DefId) -> Option<Vec<DefId>> {
    let Some(ctor) = cx.adt(d)?.ctor else {
        return Some(vec![]);
    };
    let Some(Some(Def::Fn(f))) = cx.defs.get(ctor.0 as usize) else {
        return None;
    };
    let this = f
        .params
        .first()
        .map(|p| p.local)
        .filter(|_| f.self_ty.is_some())?;
    let mut body = f.body.block.clone();
    let (ok, bases, methods) = this_uses(cx, this, &mut body, d);
    let mut memo = FxHashMap::default();
    (ok && methods.iter().all(|&m| keeps_this(cx, m, &mut memo))).then_some(bases)
}

/// How `body` uses its `this` (local `this`, of class `class`): whether only as the base of field
/// reads and writes, the receiver of `super(...)` and of method calls; the classes whose
/// constructors `super(...)` runs, and every method a call on `this` may run.
fn this_uses(
    cx: &Ctx,
    this: LocalId,
    body: &mut Block,
    class: DefId,
) -> (bool, Vec<DefId>, Vec<DefId>) {
    let mut w = ThisUses {
        cx,
        this,
        class,
        total: 0,
        allowed: 0,
        bases: vec![],
        closures: vec![],
        methods: vec![],
    };
    visit::block(body, &mut w);
    let uses = Uses {
        closures: std::mem::take(&mut w.closures),
        ..Uses::default()
    };
    let ok = w.total == w.allowed && !captured(cx, this, &uses);
    (ok, w.bases, w.methods)
}

/// Does method `m` keep its `this` to itself, as [`ctor_fresh`] requires of a constructor: used
/// only as a field base and the receiver of methods that keep it too, never stored, returned,
/// passed on or captured? A method being decided counts as keeping it (a cycle adds no escape).
fn keeps_this(cx: &Ctx, m: DefId, memo: &mut FxHashMap<DefId, bool>) -> bool {
    if let Some(&b) = memo.get(&m) {
        return b;
    }
    memo.insert(m, true);
    let b = (|| {
        let Some(Some(Def::Fn(f))) = cx.defs.get(m.0 as usize) else {
            return false;
        };
        if f.is_generator || f.is_async {
            return false;
        }
        let Some(TyKind::Adt(class, _)) = f.self_ty.map(|t| cx.ty.kind(t)) else {
            return false;
        };
        let Some(this) = f.params.first().map(|p| p.local) else {
            return false;
        };
        let mut body = f.body.block.clone();
        let (ok, _, methods) = this_uses(cx, this, &mut body, *class);
        ok && methods.iter().all(|&x| keeps_this(cx, x, memo))
    })();
    memo.insert(m, b);
    b
}

/// The methods a call of slot `slot` on a `class` object may run: the class's own and every
/// subclass's override.
fn slot_methods(cx: &Ctx, class: DefId, slot: u32) -> Vec<DefId> {
    let extends = |i: usize| {
        let mut c = DefId(i as u32);
        for _ in 0..64 {
            if c == class {
                return true;
            }
            let Some(base) = cx.adt(c).and_then(|a| a.base) else {
                return false;
            };
            let TyKind::Adt(b, _) = cx.ty.kind(base) else {
                return false;
            };
            c = *b;
        }
        false
    };
    let mut out = vec![];
    for i in 0..cx.defs.len() {
        if extends(i) {
            if let Some(&m) = cx
                .adt(DefId(i as u32))
                .and_then(|a| a.vtable.get(slot as usize))
            {
                if !out.contains(&m) {
                    out.push(m);
                }
            }
        }
    }
    out
}

/// How a constructor body uses its `this` (`ctor_fresh`).
struct ThisUses<'c, 'm> {
    cx: &'c Ctx<'m>,
    this: LocalId,
    /// The class `this` is (its methods' overrides are what a call on it may run).
    class: DefId,
    /// Every method a call on `this` may run (each must keep `this` too: [`keeps_this`]).
    methods: Vec<DefId>,
    total: u32,
    allowed: u32,
    /// Classes whose constructors `super(...)` runs on `this`.
    bases: Vec<DefId>,
    closures: Vec<DefId>,
}

impl ThisUses<'_, '_> {
    fn is_this(&self, e: &Expr) -> bool {
        match &e.kind {
            E::Local(l, _) => *l == self.this,
            E::Upcast(x) => self.is_this(x),
            _ => false,
        }
    }
}

impl VisitMut for ThisUses<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Local(l, _) if *l == self.this => self.total += 1,
            E::Closure(c) => self.closures.push(*c),
            E::Field { base, .. } if self.is_this(base) => self.allowed += 1,
            E::Call {
                callee: Callee::Def(c, _),
                args,
            } if args.first().is_some_and(|a| self.is_this(a))
                && self.cx.fn_info(*c).kind == crate::defs::FnKind::Ctor =>
            {
                if let Some(owner) = self.cx.fn_info(*c).owner {
                    self.allowed += 1;
                    self.bases.push(owner);
                }
            }
            // A method called on `this` (`this.put(…)`): allowed here, provided every method it
            // may run keeps `this` too (`ctor_fresh`). Its other arguments count as usual.
            E::Call {
                callee: Callee::Def(c, _),
                args,
            } if args.first().is_some_and(|a| self.is_this(a))
                && self.cx.fn_info(*c).kind == crate::defs::FnKind::Method =>
            {
                self.allowed += 1;
                self.methods.push(*c);
            }
            E::Call {
                callee: Callee::Virtual { slot },
                args,
            } if args.first().is_some_and(|a| self.is_this(a)) => {
                self.allowed += 1;
                let ms = slot_methods(self.cx, self.class, *slot);
                if ms.is_empty() {
                    // Not resolved: `this` escapes as far as this check knows.
                    self.allowed -= 1;
                }
                self.methods.extend(ms);
            }
            _ => {}
        }
    }
}

/// The returned values of one function, checked for freshness.
struct Returns {
    /// Functions whose results are returned.
    callees: Vec<DefId>,
    /// How often each local is returned.
    returned: FxHashMap<LocalId, u32>,
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
    inits: FxHashMap<LocalId, Vec<Expr>>,
    /// Occurrences of each local.
    total: FxHashMap<LocalId, u32>,
    /// Occurrences that neither store nor pass on its value: read through (`xs.length`,
    /// `xs[i]`, a field), the receiver of a builtin (`xs.push(x)`), assigned to.
    allowed: FxHashMap<LocalId, u32>,
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
