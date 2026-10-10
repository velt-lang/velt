//! Sync callbacks that HTTP requests call at the same time (#873): an HTTP handler only reads
//! what it captured, but a function value it reaches (`i.onChange("x")`, with
//! `i.onChange = (v) => { last = v; }`) may assign a variable that function captured itself.
//! Requests run on several threads at once and share what the handler captured, so they would
//! all assign that one variable at once. Such a handler is an error, naming the call and the
//! variable, with a fix-it that shares the variable with `shared(...)` as Node shares it.
//!
//! What a handler reaches is followed on the graph of [`super`]: the values flowing into the
//! handler `serve` passes to `Intrinsic::HttpHandler`, the captures of the closures among them,
//! and through the heap by type ([`super::types`]): a sync closure stored in a field, element or
//! map value is reached when a value the handler reaches has a type holding a function type of
//! its shape, written without generic parameters (the prelude's `resolve`, a `(T) => void`,
//! stands for no callback the user stores). A closure stored straight into an object or array
//! that one local of its function holds, and that never leaves that function, is reached only
//! when that local is ([`held_closures`]): `other.onChange = …` on an `Input` the handler never
//! sees is not one the handler's `Input` may hold. Closures made by a request (inside a reached
//! async closure) assign that request's own variables, and the standard library's closures are
//! not the user's to change, so neither is reported.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, IntTy, Lit, LocalId, Stmt, StmtKind as S, TyId,
    TyKind, UnOp,
};
use crate::visit::{self, VisitMut};

use super::graph::{Boundary, Graph, Node, Why, CROSSES, STORED};
use super::types;

/// Report the sync closures HTTP handlers reach that assign a variable they captured (module
/// docs). `flags` are the propagated node flags of [`super`] (`STORED` is used); `handlers`
/// are the sync closures passed to `serve` as handlers, checked as handlers themselves.
pub(super) fn check_handler_callbacks(
    cx: &mut Ctx,
    g: &Graph,
    flags: &[u8],
    handlers: &HashMap<DefId, Option<Why>>,
) {
    let mut work: Vec<(usize, Why)> = g
        .seeds
        .iter()
        .filter(|(_, f, _)| *f == CROSSES)
        .filter_map(|&(n, _, w)| w.filter(|w| w.boundary == Boundary::Handler).map(|w| (n, w)))
        .collect();
    let mut roots: Vec<(TyId, Why)> = g
        .roots
        .iter()
        .filter(|(_, w)| w.boundary == Boundary::Handler)
        .copied()
        .collect();
    if work.is_empty() && roots.is_empty() {
        return;
    }
    let held = held_closures(cx, g, flags);
    let mut why: Vec<Option<Why>> = vec![None; g.nodes.len()];
    let mut seen = HashSet::new();
    let mut fns: HashMap<TyId, Why> = HashMap::new();
    let mut rooted = 0;
    loop {
        while let Some((n, w)) = work.pop() {
            if why[n].is_some() {
                continue;
            }
            why[n] = Some(w);
            for &(m, site) in &g.srcs[n] {
                work.push((m, w.through(site)));
            }
            if let Some(caps) = g.captures.get(&n) {
                work.extend(caps.iter().map(|&i| (i, w)));
            }
            let ty = g.tys[n];
            if !matches!(g.nodes[n], Node::Lit(_)) && (g.unknown[n] || !types::fn_like(cx, ty)) {
                roots.push((ty, w));
            }
            for &(t, site) in &g.inflows[n] {
                if t != ty {
                    roots.push((t, w.through(site)));
                }
            }
        }
        types::crossing_fns(cx, &roots[rooted..], &mut seen, &mut fns);
        rooted = roots.len();
        for (n, node) in g.nodes.iter().enumerate() {
            if !matches!(node, Node::Lit(_)) || why[n].is_some() || flags[n] & STORED == 0 {
                continue;
            }
            if let Some(&x) = held.get(&n) {
                if let Some(w) = why[x] {
                    work.push((n, w));
                }
                continue;
            }
            let found = fns
                .iter()
                .filter(|(t, _)| !mentions_param(cx, **t) && types::may_be(cx, g.tys[n], **t))
                .map(|(_, w)| *w)
                .min_by_key(|w| (w.std, w.span.file, w.span.lo));
            if let Some(w) = found {
                work.push((n, w));
            }
        }
        if work.is_empty() {
            break;
        }
    }
    let reached: Vec<(DefId, TyId, Why)> = g
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(n, node)| match (node, why[n]) {
            (Node::Lit(c), Some(w)) => Some((*c, g.tys[n], w)),
            _ => None,
        })
        .collect();
    let requests: HashSet<DefId> = reached
        .iter()
        .filter(|(c, ..)| matches!(&cx.defs[c.0 as usize], Some(Def::Fn(f)) if f.is_async))
        .map(|(c, ..)| *c)
        .collect();
    let mut found = vec![];
    for &(c, ty, w) in &reached {
        let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
            continue;
        };
        if f.is_async
            || f.is_generator
            || handlers.contains_key(&c)
            || in_std(cx, c)
            || made_by(g, c, &requests)
        {
            continue;
        }
        for (cap, at) in assigned_captures(f) {
            let local = &f.body.locals[cap.0 as usize];
            if cx.ty.kind(local.ty) != &TyKind::Unit {
                found.push((c, ty, w, cap, local.name.clone(), local.ty, at));
            }
        }
    }
    for (c, ty, w, cap, name, cap_ty, at) in found {
        let call = call_site(cx, g, &reached, &requests, c, ty);
        let init = declaration(cx, g, c, cap);
        report(cx, &name, cap_ty, at, call, init, w);
    }
}

/// Closure literals stored straight into an object or array held by one local of the function
/// making them (`other.onChange = (v) => …`, `cbs.push(() => …)`, `const o = { f: () => … }`),
/// by literal node, with that local's node: when the local never leaves the function (it is
/// not stored, captured, returned or passed to a function of the program, only to the standard
/// library's), only that local reaches the closure, so it is reached exactly when the local is,
/// not by its type.
fn held_closures(cx: &Ctx, g: &Graph, flags: &[u8]) -> HashMap<usize, usize> {
    let mut outs: HashMap<usize, Vec<usize>> = HashMap::new();
    for (m, srcs) in g.srcs.iter().enumerate() {
        for &(n, _) in srcs {
            outs.entry(n).or_default().push(m);
        }
    }
    // Values of node `n` go only to parameters of standard library functions.
    let stays = |n: usize| {
        outs.get(&n)
            .into_iter()
            .flatten()
            .all(|&m| match g.nodes[m] {
                Node::Local(f, _) => in_std(cx, f),
                _ => false,
            })
    };
    let mut out = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = def else { continue };
        if !g.parent.values().any(|p| *p == d) || in_std(cx, d) {
            continue;
        }
        let mut sites = vec![];
        let mut block = f.body.block.clone();
        visit::block(&mut block, &mut Sites(cx, &mut sites));
        for (c, x) in sites {
            if (x.0 as usize) < f.params.len() {
                continue;
            }
            let (Some(&lit), Some(&local)) =
                (g.ids.get(&Node::Lit(c)), g.ids.get(&Node::Local(d, x)))
            else {
                continue;
            };
            if flags[local] & STORED == 0 && stays(local) && stays(lit) {
                out.insert(lit, local);
            }
        }
    }
    out
}

/// The closure literals stored straight into what a local holds, with the local (see
/// [`held_closures`]).
struct Sites<'a, 'm>(&'a Ctx<'m>, &'a mut Vec<(DefId, LocalId)>);

impl VisitMut for Sites<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        let S::Let {
            local,
            init: Some(init),
        } = &s.kind
        else {
            return;
        };
        if let E::AdtLit { fields: xs, .. } | E::ArrayLit(xs) | E::New { args: xs, .. } = &init.kind
        {
            for x in xs {
                if let E::Closure(c) = x.kind {
                    self.1.push((c, *local));
                }
            }
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Assign { place, value } => {
                if let (E::Closure(c), false) = (&value.kind, matches!(place.kind, E::Local(..))) {
                    if let Some(x) = root(place) {
                        self.1.push((*c, x));
                    }
                }
            }
            E::Call {
                callee: Callee::Def(g, _),
                args,
            } if in_std(self.0, *g) => {
                let method =
                    matches!(&self.0.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.self_ty.is_some());
                if let (true, Some(x)) = (method, args.first().and_then(root)) {
                    for a in &args[1..] {
                        if let E::Closure(c) = a.kind {
                            self.1.push((c, x));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The local a place is part of: `x`, `x.f`, `x[i]`, `x.f[i].g`.
fn root(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::Field { base, .. } | E::Index { base, .. } => root(base),
        _ => None,
    }
}

/// Is closure `c` made inside one of `requests` (a request's own closure)?
fn made_by(g: &Graph, c: DefId, requests: &HashSet<DefId>) -> bool {
    let mut d = c;
    for _ in 0..64 {
        match g.parent.get(&d) {
            Some(p) if requests.contains(p) => return true,
            Some(p) => d = *p,
            None => return false,
        }
    }
    false
}

fn in_std(cx: &Ctx, d: DefId) -> bool {
    cx.try_fn(d).is_some_and(|i| cx.scopes[i.module].is_std)
}

/// Does `t` mention a generic parameter?
fn mentions_param(cx: &Ctx, t: TyId) -> bool {
    let mut out = vec![];
    crate::types::collect_params(&cx.ty, t, &mut out);
    !out.is_empty()
}

/// The captured variables closure `f` assigns (`x = …`, `x += …`, `x++`), with the first
/// assignment of each.
fn assigned_captures(f: &FnDef) -> Vec<(LocalId, Span)> {
    let caps: HashSet<LocalId> = f.captures.iter().map(|c| c.inner).collect();
    let mut out: Vec<(LocalId, Span)> = vec![];
    let mut block = f.body.block.clone();
    visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
        let (E::Assign { place, .. } | E::CompoundAssign { place, .. }) = &e.kind else {
            return;
        };
        if let E::Local(l, _) = place.kind {
            if caps.contains(&l) && !out.iter().any(|(x, _)| *x == l) {
                out.push((l, e.span));
            }
        }
    });
    out
}

/// Where a handler calls a function value that may be closure `c` (of type `ty`), as written
/// (`i.onChange`), with the call's span: in a request's closure first, else in another closure
/// the handler reaches. A call through a variable `c` flows into is preferred, then one through
/// a function value of `c`'s type, then of its shape.
fn call_site(
    cx: &Ctx,
    g: &Graph,
    reached: &[(DefId, TyId, Why)],
    requests: &HashSet<DefId>,
    c: DefId,
    ty: TyId,
) -> Option<(String, Span)> {
    let mut order: Vec<DefId> = reached.iter().map(|(d, ..)| *d).collect();
    order.sort_by_key(|d| !requests.contains(d));
    let lit = g.ids.get(&Node::Lit(c)).copied();
    let flows_from = |n: usize| {
        let mut seen = HashSet::new();
        let mut work = vec![n];
        while let Some(m) = work.pop() {
            if Some(m) == lit {
                return true;
            }
            if seen.insert(m) {
                work.extend(g.srcs[m].iter().map(|(s, _)| *s));
            }
        }
        false
    };
    let ret = |t: TyId| match cx.ty.kind(t) {
        TyKind::FnPtr { ret, .. } => Some(*ret),
        _ => None,
    };
    let fields = field_stores(cx);
    let mut best: Option<(u8, bool, u32, String, Span)> = None;
    for d in order {
        if d == c || in_std(cx, d) {
            continue;
        }
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            continue;
        };
        let request = requests.contains(&d);
        let mut block = f.body.block.clone();
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
            let E::Call {
                callee: Callee::Indirect(callee),
                ..
            } = &e.kind
            else {
                return;
            };
            let field = match &callee.kind {
                E::Field { base, index, .. } => match cx.ty.kind(base.ty) {
                    TyKind::Adt(a, _) => fields.get(&c).map(|fs| fs.contains(&(*a, *index))),
                    _ => None,
                },
                _ => None,
            };
            let rank = match &callee.kind {
                _ if field == Some(true) => 0,
                _ if field == Some(false) => return,
                E::Local(l, _)
                    if g.ids
                        .get(&Node::Local(d, *l))
                        .is_some_and(|&n| flows_from(n)) =>
                {
                    0
                }
                _ if callee.ty == ty => 1,
                _ if types::may_be(cx, ty, callee.ty) && ret(ty) == ret(callee.ty) => 2,
                _ => return,
            };
            let Some(text) = shown(cx, f, callee) else {
                return;
            };
            let key = (rank, !request, e.span.lo);
            if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
                best = Some((rank, !request, e.span.lo, text, e.span));
            }
        });
    }
    best.map(|b| (b.3, b.4))
}

/// The fields each closure literal is assigned to (`i.onChange = (v) => …`, `{ f: () => … }`),
/// as `(class or object type, field)`.
fn field_stores(cx: &Ctx) -> HashMap<DefId, Vec<(DefId, u32)>> {
    let mut out: HashMap<DefId, Vec<(DefId, u32)>> = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let Some(Def::Fn(f)) = def else { continue };
        if in_std(cx, DefId(i as u32)) {
            continue;
        }
        let mut block = f.body.block.clone();
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| match &e.kind {
            E::Assign { place, value } => {
                if let (E::Field { base, index, .. }, E::Closure(c)) = (&place.kind, &value.kind) {
                    if let TyKind::Adt(a, _) = cx.ty.kind(base.ty) {
                        out.entry(*c).or_default().push((*a, *index));
                    }
                }
            }
            E::AdtLit { def, fields, .. } => {
                for (k, x) in fields.iter().enumerate() {
                    if let E::Closure(c) = x.kind {
                        out.entry(c).or_default().push((*def, k as u32));
                    }
                }
            }
            _ => {}
        });
    }
    out
}

/// `e` as written, for a callee: a variable, a field, an element or a call of one.
fn shown(cx: &Ctx, f: &FnDef, e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
        E::Field { base, index, .. } => {
            let b = shown(cx, f, base)?;
            let TyKind::Adt(d, _) = cx.ty.kind(base.ty) else {
                return None;
            };
            let field = cx.adt(*d)?.fields.get(*index as usize)?.name.clone();
            format!("{b}.{field}")
        }
        E::Index { base, index, .. } => {
            let i = match &index.kind {
                E::Lit(Lit::Int(n)) => n.to_string(),
                E::Local(l, _) => f.body.locals.get(l.0 as usize)?.name.clone(),
                _ => "…".into(),
            };
            format!("{}[{i}]", shown(cx, f, base)?)
        }
        E::Call {
            callee: Callee::Def(g, _),
            args,
        } => {
            let name = match &cx.defs[g.0 as usize] {
                Some(Def::Fn(gf)) => gf.name.rsplit("::").next()?.to_string(),
                _ => return None,
            };
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{name}({dots})")
        }
        E::Call {
            callee: Callee::Indirect(inner),
            args,
        } => {
            let dots = if args.is_empty() { "" } else { "…" };
            format!("{}({dots})", shown(cx, f, inner)?)
        }
        E::Cast(x) | E::Upcast(x) | E::UnwrapSome(x, _) => shown(cx, f, x)?,
        _ => return None,
    })
}

/// The initializer of the variable closure `c` captured as `cap`, as written (`""`, `0`):
/// found on its declaration in the function making `c` (or the one making that, for a variable
/// captured on the way). `None` when it is not a literal.
fn declaration(cx: &Ctx, g: &Graph, c: DefId, cap: LocalId) -> Option<String> {
    let (mut d, mut l) = (c, cap);
    for _ in 0..64 {
        let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
            return None;
        };
        let outer = f.captures.iter().find(|k| k.inner == l)?.outer;
        let p = *g.parent.get(&d)?;
        let Some(Def::Fn(pf)) = &cx.defs[p.0 as usize] else {
            return None;
        };
        if pf.captures.iter().any(|k| k.inner == outer) {
            (d, l) = (p, outer);
            continue;
        }
        let mut block = pf.body.block.clone();
        let mut v = Decl(outer, None);
        visit::block(&mut block, &mut v);
        return v.1.flatten();
    }
    None
}

/// Finds the initializer of a local's `let`, as written when it is a literal.
struct Decl(LocalId, Option<Option<String>>);

impl VisitMut for Decl {
    fn stmt(&mut self, s: &mut Stmt) {
        if let S::Let { local, init } = &s.kind {
            if *local == self.0 && self.1.is_none() {
                self.1 = Some(init.as_ref().and_then(literal));
            }
        }
    }
}

fn literal(e: &Expr) -> Option<String> {
    Some(match &e.kind {
        E::Lit(Lit::Str(s)) => {
            let mut out = String::from("\"");
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        E::Lit(Lit::Int(n)) => n.to_string(),
        E::Lit(Lit::Float(x)) => {
            let s = x.to_string();
            s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
        }
        E::Lit(Lit::Bool(b)) => b.to_string(),
        E::Lit(Lit::Null) => "null".into(),
        E::Unary {
            op: UnOp::Neg,
            expr,
        } => format!("-{}", literal(expr)?),
        E::ArrayLit(xs) if xs.is_empty() => "[]".into(),
        E::Cast(x) | E::WrapSome(x) | E::Upcast(x) => literal(x)?,
        _ => return None,
    })
}

fn report(
    cx: &mut Ctx,
    name: &str,
    ty: TyId,
    at: Span,
    call: Option<(String, Span)>,
    init: Option<String>,
    why: Why,
) {
    let mut d = match &call {
        Some((callee, span)) => Diagnostic::error(
            format!("this handler calls `{callee}`, which changes `{name}`; requests run at the same time"),
            *span,
        )
        .with_label(at, format!("`{name}` is changed here")),
        None => Diagnostic::error(
            format!("an HTTP handler can call this function, which changes `{name}`; requests run at the same time"),
            at,
        )
        .with_label(why.span, "the handler is passed to `serve` here"),
    };
    let init = init.unwrap_or_else(|| "…".into());
    // `shared` alone holds a 64-bit integer (`add`, `get`, `set`); any other value goes in a
    // `Mutex`, and one that is not copied (a string, an object) in an object there, since a
    // `with` callback can replace the fields of what it gets but not the value itself.
    let atomic = matches!(
        cx.ty.kind(ty),
        TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize)
    );
    let (decl, how) = if atomic {
        (
            format!("shared({init})"),
            format!("then read it with `{name}.get()` and change it with `{name}.set(v)` or `{name}.add(1)`"),
        )
    } else if cx.is_copy(ty) {
        (
            format!("shared(new Mutex({init}))"),
            format!("then read it with `{name}.with((v) => v)` and change it with `{name}.with((v) => {{ v = … }})`"),
        )
    } else {
        (
            format!("shared(new Mutex({{ value: {init} }}))"),
            format!("then read it with `{name}.with((v) => v.value)` and change it with `{name}.with((v) => {{ v.value = … }})`"),
        )
    };
    d = d
        .with_note(format!(
            "fix: const {name} = {decl}  // one value shared by every request, as in Node"
        ))
        .with_note(how);
    cx.error(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    fn e(kind: E) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::new(FileId(0), 0, 0),
        }
    }

    #[test]
    fn literal_initializers_are_shown_as_written() {
        let s = |x: &str| literal(&e(E::Lit(Lit::Str(x.into()))));
        assert_eq!(s("").as_deref(), Some("\"\""));
        assert_eq!(s("a\"b\\").as_deref(), Some("\"a\\\"b\\\\\""));
        assert_eq!(literal(&e(E::Lit(Lit::Int(7)))).as_deref(), Some("7"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(1.5)))).as_deref(), Some("1.5"));
        assert_eq!(literal(&e(E::Lit(Lit::Float(2.0)))).as_deref(), Some("2"));
        let neg = E::Unary {
            op: UnOp::Neg,
            expr: Box::new(e(E::Lit(Lit::Int(3)))),
        };
        assert_eq!(literal(&e(neg)).as_deref(), Some("-3"));
        assert_eq!(literal(&e(E::ArrayLit(vec![]))).as_deref(), Some("[]"));
        let call = E::ArrayLit(vec![e(E::Lit(Lit::Int(1)))]);
        assert_eq!(literal(&e(call)), None);
    }
}
