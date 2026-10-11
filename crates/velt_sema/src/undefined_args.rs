//! `T | undefined` and `T | null` as type arguments (issue #930, phase 1).
//!
//! In this phase `T | undefined` and `T | null` share one layout (one marker for the nothing),
//! and an option of an option is the inner option (`TyTable::intern`). Generic code that keeps
//! a `U | null` of a type parameter `U` would therefore hold its own `null` and a
//! `U = T | undefined` value's `undefined` in one marker, and could not tell them apart (a
//! `Map` entry holding `undefined` would read as a missing one); the same holds the other way
//! round, for a `U | undefined` at `U = T | null`. Until the three-state encoding (the design
//! note's marker `2`) lands, such an instantiation is reported, at the place that makes it: a
//! call of a generic function, or a value of a generic type, whose definition keeps an option
//! with the other nothing of the parameter that receives the argument.
//!
//! What a definition keeps follows calls: a generic function that passes its own parameter on
//! to a callee that keeps `U | null` of it keeps it too (to a fixpoint over the call graph),
//! as do closures (they share their function's type parameters), a generic type's methods (for
//! a value of the type) and the generic types a definition uses. Programs that never write
//! `undefined` have no `T | undefined` type, so this pass returns at once for them.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Nothing, TyId, TyKind};
use crate::types::children;
use crate::visit;

/// Which nothings options of a type parameter have: `NULL` and/or `UNDEFINED`.
type Mask = u8;
const NULL: Mask = 1;
const UNDEFINED: Mask = 2;

fn bits(n: Nothing) -> Mask {
    match n {
        Nothing::Null => NULL,
        Nothing::Undefined => UNDEFINED,
        Nothing::Both => NULL | UNDEFINED,
    }
}

/// `masks[d][i] |= masks[from.0][from.1]` for `from` = (`def`, parameter), into = (`def`,
/// parameter); `None` as the parameter: every parameter (a closure).
struct Edge {
    from: (DefId, Option<u32>),
    into: DefId,
    into_param: Option<u32>,
}

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let n = cx.ty.table.len() as u32;
    let any = (0..n).any(|i| matches!(cx.ty.kind(TyId(i)), TyKind::Option(_, Nothing::Undefined)));
    if !any {
        return;
    }
    let mut masks: HashMap<DefId, Vec<Mask>> = HashMap::new();
    let mut edges: Vec<Edge> = vec![];
    // (definition, type arguments, span) of every instantiation the bodies make.
    let mut uses: Vec<(DefId, Vec<TyId>, Span)> = vec![];
    let mut ty_uses: Vec<(TyId, Span)> = vec![];
    let mut seen_tys: HashSet<TyId> = HashSet::new();
    for i in 0..cx.defs.len() {
        let d = DefId(i as u32);
        let (own, body) = match cx.defs.get_mut(i) {
            Some(Some(Def::Fn(f))) => fn_facts(f, &mut uses, &mut ty_uses, &mut seen_tys),
            _ => match cx.adt(d) {
                Some(a) if !a.generics.names.is_empty() => {
                    (a.fields.iter().map(|f| f.ty).collect(), vec![])
                }
                _ => continue,
            },
        };
        masks.insert(d, direct_mask(cx, &own));
        for (callee, targs, closure) in body {
            if closure {
                edges.push(Edge {
                    from: (callee, None),
                    into: d,
                    into_param: None,
                });
                continue;
            }
            for (j, t) in targs.iter().enumerate() {
                if let TyKind::Param(p) = cx.ty.kind(*t) {
                    edges.push(Edge {
                        from: (callee, Some(j as u32)),
                        into: d,
                        into_param: Some(*p),
                    });
                }
            }
        }
        // A method keeps for its type what it keeps for the type's parameters.
        let self_ty = match cx.defs.get(i) {
            Some(Some(Def::Fn(f))) => f.self_ty,
            _ => None,
        };
        if let Some(TyKind::Adt(a, args)) = self_ty.map(|t| cx.ty.kind(t).clone()) {
            for (j, x) in args.iter().enumerate() {
                if let TyKind::Param(p) = cx.ty.kind(*x) {
                    edges.push(Edge {
                        from: (d, Some(*p)),
                        into: a,
                        into_param: Some(j as u32),
                    });
                }
            }
        }
        // The generic types the definition uses keep their options of its parameters.
        let mut walked = HashSet::new();
        let mut stack = own_types(cx, d);
        while let Some(t) = stack.pop() {
            if !walked.insert(t) {
                continue;
            }
            let k = cx.ty.kind(t).clone();
            if let TyKind::Adt(a, args) = &k {
                for (j, x) in args.iter().enumerate() {
                    if let TyKind::Param(p) = cx.ty.kind(*x) {
                        edges.push(Edge {
                            from: (*a, Some(j as u32)),
                            into: d,
                            into_param: Some(*p),
                        });
                    }
                }
            }
            stack.extend(children(&k));
        }
    }
    propagate(&mut masks, &edges);
    for (t, span) in ty_uses {
        let mut stack = vec![t];
        let mut walked = HashSet::new();
        while let Some(t) = stack.pop() {
            if !walked.insert(t) {
                continue;
            }
            let k = cx.ty.kind(t).clone();
            if let TyKind::Adt(d, args) = &k {
                if !args.is_empty() {
                    uses.push((*d, args.clone(), span));
                }
            }
            stack.extend(children(&k));
        }
    }
    let mut reported: HashSet<Span> = HashSet::new();
    for (d, targs, span) in uses {
        let Some(m) = masks.get(&d) else { continue };
        let bad = targs.iter().enumerate().find_map(|(i, t)| {
            let have = cx.ty.nothing_of(*t)?;
            let kept = m.get(i).copied().unwrap_or(0) & !bits(have);
            (kept != 0).then_some((*t, have, kept))
        });
        let Some((t, _, kept)) = bad else { continue };
        if !reported.insert(span) {
            continue;
        }
        let tn = cx.display(t);
        let name = def_name(cx, d);
        let other = if kept & NULL != 0 { "null" } else { "undefined" };
        cx.error(
            Diagnostic::error(
                format!("`{tn}` as a type argument of `{name}` is not supported yet"),
                span,
            )
            .with_note(format!(
                "`{name}` keeps a `T | {other}` of that type parameter, and a type that holds both `null` and `undefined` is not supported yet"
            )),
        );
    }
}

/// What function `f` uses: the types it keeps directly (signature, locals, expressions, type
/// arguments it passes) and, per call, function value or closure, (callee, type arguments, is a
/// closure). Also records `f`'s instantiations in `uses` and its expression types in `ty_uses`.
#[allow(clippy::type_complexity)]
fn fn_facts(
    f: &mut crate::hir::FnDef,
    uses: &mut Vec<(DefId, Vec<TyId>, Span)>,
    ty_uses: &mut Vec<(TyId, Span)>,
    seen_tys: &mut HashSet<TyId>,
) -> (Vec<TyId>, Vec<(DefId, Vec<TyId>, bool)>) {
    let mut own: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
    own.push(f.ret);
    own.extend(f.body.locals.iter().map(|l| l.ty));
    let mut body: Vec<(DefId, Vec<TyId>, bool)> = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        own.push(e.ty);
        match &e.kind {
            E::Call {
                callee: Callee::Def(c, targs),
                ..
            }
            | E::FnRef(c, targs) => {
                if !targs.is_empty() {
                    uses.push((*c, targs.clone(), e.span));
                    own.extend(targs.iter().copied());
                    body.push((*c, targs.clone(), false));
                }
            }
            E::Closure(c) => body.push((*c, vec![], true)),
            _ => {}
        }
        if seen_tys.insert(e.ty) {
            ty_uses.push((e.ty, e.span));
        }
    });
    (own, body)
}

/// Types definition `d` mentions in its signature, locals and fields (not its body's
/// expressions, which `fn_facts` walked).
fn own_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.defs.get(d.0 as usize) {
        Some(Some(Def::Fn(f))) => {
            let mut v: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
            v.push(f.ret);
            v.extend(f.body.locals.iter().map(|l| l.ty));
            v
        }
        _ => cx
            .adt(d)
            .map(|a| a.fields.iter().map(|f| f.ty).collect())
            .unwrap_or_default(),
    }
}

/// For each type parameter: the nothings of the options of it among `tys` (and their parts).
fn direct_mask(cx: &Ctx, tys: &[TyId]) -> Vec<Mask> {
    let mut out: Vec<Mask> = vec![];
    let mut walked = HashSet::new();
    let mut stack = tys.to_vec();
    while let Some(t) = stack.pop() {
        if !walked.insert(t) {
            continue;
        }
        let k = cx.ty.kind(t).clone();
        if let TyKind::Option(p, n) = k {
            if let TyKind::Param(i) = cx.ty.kind(p) {
                let i = *i as usize;
                if out.len() <= i {
                    out.resize(i + 1, 0);
                }
                out[i] |= bits(n);
            }
        }
        stack.extend(children(&k));
    }
    out
}

/// Pass the masks along `edges` until nothing changes.
fn propagate(masks: &mut HashMap<DefId, Vec<Mask>>, edges: &[Edge]) {
    let get = |masks: &HashMap<DefId, Vec<Mask>>, d: DefId, i: usize| {
        masks.get(&d).and_then(|m| m.get(i)).copied().unwrap_or(0)
    };
    loop {
        let mut changed = false;
        for e in edges {
            let (from, from_param) = e.from;
            let pairs: Vec<(usize, Mask)> = match (from_param, e.into_param) {
                (Some(j), Some(i)) => vec![(i as usize, get(masks, from, j as usize))],
                _ => {
                    let src = masks.get(&from).cloned().unwrap_or_default();
                    src.into_iter().enumerate().collect()
                }
            };
            for (i, m) in pairs {
                if m == 0 {
                    continue;
                }
                let into = masks.entry(e.into).or_default();
                if into.len() <= i {
                    into.resize(i + 1, 0);
                }
                if into[i] | m != into[i] {
                    into[i] |= m;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// `d`'s name without its module path (`Map.set`).
fn def_name(cx: &Ctx, d: DefId) -> String {
    let full = match cx.defs.get(d.0 as usize) {
        Some(Some(Def::Fn(f))) => f.name.as_str(),
        _ => match cx.adt(d) {
            Some(a) => a.name.as_str(),
            None => "this definition",
        },
    };
    full.rsplit("::").next().unwrap_or(full).to_string()
}
