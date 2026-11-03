//! Preparing a body for [`super`] once: what each local may be, and its stores by key, calls and
//! closures with what each operand may be; and the cheap walk that finds which functions a
//! crossing reaches may store at all.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, Lit, LocalId};
use crate::ownership::local_closures::walk::{self, Visit};

use super::super::changes::{callee_name, Memo};
use super::super::fed::{adt_of, own};
use super::super::reach::{constructor, plain};
use super::super::targets::{method_targets, targets_for, Literals};
use super::apply::Shapes;
use super::{Arg, Ev, Key, Prepared, Run, Site};

/// The functions whose summary may not be empty: those that store through an `any`, and those
/// that call, construct, make or may call through a function value one of them, found
/// backwards from the stores over the calls the scan recorded (super::super::targets), so that
/// a program whose stores by key are far from its crossings costs little. std's bodies are not
/// followed.
pub(super) fn may_store(cx: &Ctx, lits: &Literals) -> FxHashSet<DefId> {
    let mut may: FxHashSet<DefId> = lits.store_sites.iter().copied().collect();
    if may.is_empty() {
        return may;
    }
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    for &(c, g) in &lits.calls {
        callers.entry(g).or_default().push(c);
    }
    for (c, callee, recv) in &lits.dispatch {
        for (g, _) in targets_for(cx, lits, callee, *recv).into_iter().flatten() {
            callers.entry(g).or_default().push(*c);
        }
    }
    // A call through a function value may run any of the program's: once one of those may
    // store, every such caller may.
    let values: FxHashSet<DefId> = lits.values().iter().copied().collect();
    let mut values_done = false;
    let mut work: Vec<DefId> = may.iter().copied().collect();
    while let Some(g) = work.pop() {
        if !values_done && values.contains(&g) {
            values_done = true;
            for &c in &lits.through_values {
                if may.insert(c) {
                    work.push(c);
                }
            }
        }
        for &c in callers.get(&g).into_iter().flatten() {
            if may.insert(c) {
                work.push(c);
            }
        }
    }
    may
}

/// Body `f` prepared: what each local may be, then its stores, calls and closures with what each
/// operand may be.
pub(super) fn prepare(cx: &mut Ctx, run: &mut Run, f: &FnDef) -> Prepared {
    let srcs = sources(cx, f, &mut run.memo);
    let names: Vec<String> = f.body.locals.iter().map(|l| l.name.clone()).collect();
    let who = display(&f.name);
    let mut p = Prep {
        cx,
        memo: &mut run.memo,
        lits: run.lits,
        srcs: &srcs,
        names: &names,
        who: &who,
        shapes: &mut run.shapes,
        events: vec![],
    };
    walk::block(&f.body.block, &mut p);
    Prepared {
        params: f.params.iter().map(|p| p.ty).collect(),
        events: p.events,
    }
}

/// The walk preparing one body: each call, `new`, closure and store by key becomes an event as
/// it is reached (nothing is copied out of the body).
struct Prep<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    memo: &'a mut Memo,
    lits: &'a Literals,
    srcs: &'a FxHashMap<LocalId, Vec<Arg>>,
    names: &'a [String],
    who: &'a str,
    shapes: &'a mut Shapes,
    events: Vec<Ev>,
}

impl Prep<'_, '_> {
    fn at(&self, e: &Expr, what: String) -> Site {
        Site {
            span: e.span,
            text: match self.who {
                "a closure" => format!("`{what}` in a closure"),
                who => format!("`{what}` in `{who}`"),
            },
        }
    }

    fn arg(&mut self, e: &Expr) -> Vec<Arg> {
        args_in(self.cx, self.srcs, e, self.memo)
    }

    fn args(&mut self, xs: &[Expr]) -> Vec<Vec<Arg>> {
        args_all(self.cx, self.srcs, xs, self.memo)
    }
}

impl Visit for Prep<'_, '_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::New { def, args, .. } => {
                let Some(ctor) = constructor(self.cx, *def) else {
                    return;
                };
                let mut actual = vec![vec![Arg::New(e.ty)]];
                actual.extend(self.args(args));
                let name = self
                    .cx
                    .adt(*def)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let site = self.at(e, format!("new {name}(…)"));
                let targets = vec![(ctor, self.shapes.id(&actual))];
                self.events.push(Ev::Call { targets, site });
            }
            E::Closure(k) => {
                let caps: Vec<LocalId> = match &self.cx.defs[k.0 as usize] {
                    Some(Def::Fn(kf)) => kf.captures.iter().map(|c| c.outer).collect(),
                    _ => vec![],
                };
                let caps: Vec<Vec<Arg>> = (caps.iter())
                    .map(|o| {
                        self.srcs
                            .get(o)
                            .cloned()
                            .unwrap_or_else(|| vec![Arg::Other])
                    })
                    .collect();
                let site = self.at(e, "(…) => …".to_string());
                let caps = self.shapes.id(&caps);
                self.events.push(Ev::Closure { k: *k, caps, site });
            }
            E::Call { callee, args } => match callee {
                Callee::Intrinsic(Intrinsic::AnySet) if args.len() >= 3 => {
                    let key = match &args[1].kind {
                        E::Lit(Lit::Str(s)) => Key::Name(s.clone()),
                        _ => Key::Any,
                    };
                    let target = self.arg(&args[0]);
                    let value = self.arg(&args[2]);
                    let place = match &key {
                        Key::Name(n) => format!("{}.{n} = …", short(self.names, &args[0])),
                        Key::Any => format!("{}[…] = …", short(self.names, &args[0])),
                    };
                    let site = self.at(e, place);
                    self.events.push(Ev::Store {
                        target,
                        key,
                        value,
                        site,
                    });
                }
                Callee::Def(d, _) => {
                    let name = callee_name(self.cx, *d).map(|n| n.0).unwrap_or_default();
                    let actual = self.args(args);
                    let site = self.at(e, format!("{name}(…)"));
                    let targets = vec![(*d, self.shapes.id(&actual))];
                    self.events.push(Ev::Call { targets, site });
                }
                Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                    let Some(targets) = method_targets(self.cx, self.lits, callee, args) else {
                        return;
                    };
                    let actual = self.args(args);
                    let recv = args
                        .first()
                        .map(|a| short(self.names, a))
                        .unwrap_or_default();
                    let site = self.at(e, format!("{recv}.…(…)"));
                    // A literal's function takes no `this`: its arguments start after it.
                    let targets = (targets.into_iter())
                        .map(|(g, skip)| (g, self.shapes.id(&actual[skip.min(actual.len())..])))
                        .collect();
                    self.events.push(Ev::Call { targets, site });
                }
                Callee::Indirect(fv) => {
                    let fns = self.arg(fv);
                    let actual = self.args(args);
                    let site = self.at(e, format!("{}(…)", short(self.names, fv)));
                    let actual = self.shapes.id(&actual);
                    self.events.push(Ev::Value { fns, actual, site });
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// What each local of `f` may be ([`Arg`]): its parameters are themselves; a local bound to a
/// value computed from others is what that value may be.
fn sources(cx: &mut Ctx, f: &FnDef, memo: &mut Memo) -> FxHashMap<LocalId, Vec<Arg>> {
    let mut srcs: FxHashMap<LocalId, Vec<Arg>> = FxHashMap::default();
    for (i, p) in f.params.iter().enumerate() {
        srcs.insert(p.local, vec![Arg::Param(i as u32)]);
    }
    let mut binds = vec![];
    walk::block(
        &f.body.block,
        &mut Binds {
            out: &mut binds,
            cx,
            locals: &f.body.locals,
        },
    );
    // Each pass can only add to finite sets, so this reaches the fixpoint.
    loop {
        let mut changed = false;
        for (l, from) in &binds {
            let got = args_in(cx, &srcs, from, memo);
            let entry = srcs.entry(*l).or_default();
            for a in got {
                if !entry.contains(&a) {
                    entry.push(a);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    srcs
}

/// What each of `xs` may be.
fn args_all(
    cx: &mut Ctx,
    srcs: &FxHashMap<LocalId, Vec<Arg>>,
    xs: &[Expr],
    memo: &mut Memo,
) -> Vec<Vec<Arg>> {
    xs.iter().map(|a| args_in(cx, srcs, a, memo)).collect()
}

/// What the value `e` computes may be ([`Arg`]).
fn args_in(
    cx: &mut Ctx,
    srcs: &FxHashMap<LocalId, Vec<Arg>>,
    e: &Expr,
    memo: &mut Memo,
) -> Vec<Arg> {
    match &e.kind {
        E::ToDyn { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::Cast(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _) => args_in(cx, srcs, x, memo),
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::AnyTo | Intrinsic::AnyToExact),
            args,
        } if !args.is_empty() => args_in(cx, srcs, &args[0], memo),
        E::Block(b) => match &b.value {
            Some(v) => args_in(cx, srcs, v, memo),
            None => vec![Arg::Own],
        },
        // Either branch (a nullable value converted to an `any`: `null`, or the value).
        E::If { then, els, .. } => {
            let mut out = args_in(cx, srcs, then, memo);
            for a in args_in(cx, srcs, els, memo) {
                if !out.contains(&a) {
                    out.push(a);
                }
            }
            out
        }
        E::Match { arms, .. } => {
            let mut out = vec![];
            for arm in arms {
                for a in args_in(cx, srcs, &arm.body, memo) {
                    if !out.contains(&a) {
                        out.push(a);
                    }
                }
            }
            out
        }
        E::Local(l, _) => srcs.get(l).cloned().unwrap_or_else(|| vec![Arg::Ty(e.ty)]),
        E::Closure(k) | E::FnRef(k, _) => vec![Arg::Fns(vec![*k])],
        _ if own(cx, e, memo) => match adt_of(cx, e.ty) {
            // A new object: stored, it is the request's; stored into, its fields are fed.
            Some(_) => vec![Arg::New(e.ty)],
            None => vec![Arg::Own],
        },
        _ => vec![Arg::Ty(e.ty)],
    }
}

/// A function's name as an error shows it (`ThroughAny.fill`, not `main::ThroughAny.fill`).
pub(super) fn display(name: &str) -> String {
    let short = name.rsplit("::").next().unwrap_or(name);
    // A closure has no name of its own (`{closure#12}`).
    match short.starts_with('{') {
        true => "a closure".to_string(),
        false => short.to_string(),
    }
}

/// A short form of operand `e` for a store's site: a local's name, or `…`.
fn short(names: &[String], e: &Expr) -> String {
    match &e.kind {
        E::ToDyn { expr: x, .. } | E::Upcast(x) | E::Cast(x) => short(names, x),
        E::Local(l, _) => match names.get(l.0 as usize) {
            Some(n) if !n.starts_with('<') && !n.starts_with('#') => n.clone(),
            _ => "…".to_string(),
        },
        _ => "…".to_string(),
    }
}

/// Locals bound to a whole value: `let`, an assignment to the local. A loop variable or a
/// destructured part is a part, not followed here (its type decides). A local that holds no
/// reference (a number, a string) is left out.
struct Binds<'a, 'm> {
    out: &'a mut Vec<(LocalId, Expr)>,
    cx: &'a mut Ctx<'m>,
    locals: &'a [crate::hir::LocalDef],
}

impl Binds<'_, '_> {
    fn bind(&mut self, l: LocalId, init: &Expr) {
        let ty = self.locals[l.0 as usize].ty;
        if !plain(self.cx, ty) {
            self.out.push((l, init.clone()));
        }
    }
}

impl Visit for Binds<'_, '_> {
    fn stmt(&mut self, s: &crate::hir::Stmt) {
        if let crate::hir::StmtKind::Let {
            local,
            init: Some(init),
        } = &s.kind
        {
            self.bind(*local, init);
        }
    }

    fn expr(&mut self, e: &Expr) {
        if let E::Assign { place, value } = &e.kind {
            if let E::Local(l, _) = place.kind {
                self.bind(l, value);
            }
        }
    }
}
