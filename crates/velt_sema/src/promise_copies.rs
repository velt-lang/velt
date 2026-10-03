//! Copies of promises through generic code. A promise has one owner and can't be copied, so a
//! share or clone of a value that holds one panics at run time ("a promise cannot be copied",
//! the clone glue's backstop). Sema never shares a promise it can see; this pass finds the copies
//! a generic function makes of its type parameters (`m.get(k)`, `arr.at(i)`, `arr.flat()`,
//! `Object.values(r)`, a user's `first<T>(xs: T[])`, …) and reports the calls that bind such a
//! parameter to a promise-holding type.
//!
//! After all bodies and the ownership passes (which add shares), each function's facts are
//! collected: the types its `Share` / `Clone` intrinsics copy, and its calls of generic
//! functions. A fixed point over the call graph (like the record-key check) gives each function
//! the generic types it copies, substituted at every call; a call that makes one of them a
//! promise-holding type is reported there.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{instantiate, Dispatch};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, StmtKind, TyId, TyKind};
use crate::types::collect_params;
use crate::visit::{self, VisitMut};

/// How every note starts: TypeScript allows the read, and why Velt doesn't.
pub(crate) const WHY: &str =
    "TypeScript allows this, but a Velt promise has one owner (`await` takes \
     its result) and can't be copied or shared (shared promises are planned: #212)";

/// A copied type and whether the copy is deep (a clone copies arrays and objects too; a share
/// only copies values in place and adds a reference to counted objects).
type Copy = (TyId, bool);

/// The facts of one function body.
#[derive(Default)]
struct Facts {
    copies: Vec<Copy>,
    /// Generic calls: callee, type arguments, span of the call.
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    closures: Vec<DefId>,
    /// Class values whose methods are called dynamically (virtual calls, interface values).
    dispatch: Vec<(TyId, Span)>,
    /// The methods those reach, with the class's type arguments.
    dyn_calls: Vec<(DefId, Vec<TyId>, Span)>,
    /// Spans of `for...of` iterables (a call there is "iterating").
    loops: HashSet<Span>,
}

struct Collect<'a> {
    facts: &'a mut Facts,
    ctors: &'a HashMap<DefId, DefId>,
}

impl VisitMut for Collect<'_> {
    fn stmt(&mut self, s: &mut crate::hir::Stmt) {
        if let StmtKind::ForOf { iter, .. } = &s.kind {
            self.facts.loops.insert(iter.span);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(i @ (Intrinsic::Share | Intrinsic::Clone)),
                args,
            } => {
                if let Some(a) = args.first() {
                    self.facts.copies.push((a.ty, *i == Intrinsic::Clone));
                }
            }
            E::Call {
                callee: Callee::Def(d, targs),
                ..
            } if !targs.is_empty() => self.facts.calls.push((*d, targs.clone(), e.span)),
            // A constructor copies from its arguments (`new Map(entries)`); without any (only
            // empty array literals) there is nothing to copy.
            E::New {
                def,
                type_args,
                args,
            } if !type_args.is_empty() && !args.iter().all(is_empty_array) => {
                if let Some(ctor) = self.ctors.get(def) {
                    self.facts.calls.push((*ctor, type_args.clone(), e.span));
                }
            }
            E::Closure(c) => self.facts.closures.push(*c),
            // A generic function used as a value: its copies happen wherever it is called.
            E::FnRef(d, targs) if !targs.is_empty() => {
                self.facts.calls.push((*d, targs.clone(), e.span));
            }
            // Dynamic dispatch: the methods a class value's vtable can reach.
            E::ToDyn { expr, .. } => self.facts.dispatch.push((expr.ty, e.span)),
            E::Call {
                callee: Callee::Virtual { .. },
                args,
            } => {
                if let Some(recv) = args.first() {
                    self.facts.dispatch.push((recv.ty, e.span));
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn check(cx: &mut Ctx) {
    let mut facts = collect(cx);
    // Dynamic dispatch becomes calls of every method the value's class can reach.
    let mut dispatch = Dispatch::default();
    for (_, fx) in &mut facts {
        for (t, span) in std::mem::take(&mut fx.dispatch) {
            for (m, args) in dispatch.targets(cx, t) {
                fx.dyn_calls.push((m, args, span));
            }
        }
    }
    // The generic types each function copies (types that mention its type parameters).
    let mut needs: HashMap<DefId, Vec<Copy>> = HashMap::new();
    // Concrete promise-holding copies found at calls: (caller, call span) -> (copied type, callee).
    let mut found: Vec<(DefId, Span, TyId, DefId)> = vec![];
    let mut changed = true;
    while changed {
        changed = false;
        for (f, fx) in &facts {
            let mut reqs: Vec<(Copy, Option<(Span, DefId)>)> =
                fx.copies.iter().map(|c| (*c, None)).collect();
            for (d, targs, span) in &fx.calls {
                for (t, deep) in needs.get(d).cloned().unwrap_or_default() {
                    reqs.push(((cx.ty.subst(t, targs), deep), Some((*span, *d))));
                }
            }
            for (m, args, span) in &fx.dyn_calls {
                for (t, deep) in needs.get(m).cloned().unwrap_or_default() {
                    if let Some(t) = instantiate(cx, t, args) {
                        reqs.push(((t, deep), Some((*span, *m))));
                    }
                }
            }
            for c in &fx.closures {
                for copy in needs.get(c).cloned().unwrap_or_default() {
                    reqs.push((copy, None));
                }
            }
            for ((t, deep), at) in reqs {
                if is_generic(cx, t) {
                    let n = needs.entry(*f).or_default();
                    if !n.contains(&(t, deep)) {
                        n.push((t, deep));
                        changed = true;
                    }
                } else if let Some((span, callee)) = at {
                    if copies_promise(cx, t, deep, &mut vec![])
                        && !found.iter().any(|(g, s, _, _)| *g == *f && *s == span)
                    {
                        found.push((*f, span, t, callee));
                    }
                }
            }
        }
    }
    found.sort_by_key(|(_, s, _, _)| (s.file, s.lo));
    for (f, span, t, callee) in found {
        let iterating = facts
            .iter()
            .find(|(g, _)| *g == f)
            .is_some_and(|(_, fx)| fx.loops.contains(&span));
        report(cx, span, t, callee, iterating);
    }
}

/// `[]` (a defaulted `entries` argument): holds nothing to copy.
fn is_empty_array(e: &Expr) -> bool {
    matches!(&e.kind, E::ArrayLit(xs) if xs.is_empty())
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Facts)> {
    let ctors: HashMap<DefId, DefId> = (0..cx.info.len())
        .filter_map(|i| match &cx.info[i] {
            DefInfo::Adt(a) => a.ctor.map(|c| (DefId(i as u32), c)),
            _ => None,
        })
        .collect();
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut facts = Facts::default();
        // A capture that shares the enclosing variable copies it, without an intrinsic.
        for c in &f.captures {
            if c.share {
                facts
                    .copies
                    .push((f.body.locals[c.inner.0 as usize].ty, false));
            }
        }
        visit::block(
            &mut f.body.block,
            &mut Collect {
                facts: &mut facts,
                ctors: &ctors,
            },
        );
        out.push((DefId(i as u32), facts));
    }
    out
}

fn is_generic(cx: &Ctx, t: TyId) -> bool {
    let mut ps = vec![];
    collect_params(&cx.ty, t, &mut ps);
    !ps.is_empty()
}

/// Does copying a `t` copy a promise? A share copies values in place (options, tuples, unions,
/// structs) and adds a reference to counted objects (arrays, classes); a clone (`deep`) copies
/// those too.
fn copies_promise(cx: &mut Ctx, t: TyId, deep: bool, stack: &mut Vec<TyId>) -> bool {
    if stack.contains(&t) {
        return false;
    }
    stack.push(t);
    let parts: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Promise(..) => {
            stack.pop();
            return true;
        }
        TyKind::Option(e) => vec![e],
        TyKind::Tuple(ts) => ts,
        TyKind::Array(e) if deep => vec![e],
        TyKind::Adt(d, args) => {
            let class = cx
                .adt(d)
                .is_some_and(|a| a.kind == crate::hir::AdtKind::Class);
            if class && !deep {
                vec![]
            } else {
                let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                    DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
                    _ => vec![],
                };
                tys.into_iter().map(|f| cx.ty.subst(f, &args)).collect()
            }
        }
        _ => cx.union_members(t).unwrap_or_default(),
    };
    let found = parts
        .into_iter()
        .any(|p| copies_promise(cx, p, deep, stack));
    stack.pop();
    found
}

/// What the call reads from: `(" out of the `Map`", true)` for a `Map` method, and so on.
fn container(name: &str) -> (&'static str, bool) {
    let short = name.rsplit("::").next().unwrap_or("");
    let owner = short.rsplit_once('.').map_or("", |(o, _)| o);
    match owner {
        "Map" => (" out of the `Map`", true),
        "Record" => (" out of the `Record`", true),
        "Set" => (" out of the `Set`", false),
        o if o.ends_with("[]") => (" out of the array", false),
        _ => ("", false),
    }
}

/// How the program wrote the call: the method or function name, or the syntax the compiler
/// turned into a call of a std helper.
fn written(name: &str) -> String {
    let short = name.rsplit("::").next().unwrap_or(name);
    let method = short.rsplit('.').next().unwrap_or(short);
    match method {
        "__get" | "__at" => "reading `r[k]`".into(),
        "__values" => "`Object.values`".into(),
        "__entries" => "`Object.entries`".into(),
        "__arrayFilled" => "`new Array(n).fill(v)`".into(),
        "constructor" => match short.rsplit_once('.') {
            Some((owner, _)) => format!("`new {owner}(…)`"),
            None => "the constructor".into(),
        },
        m => format!("`{m}`"),
    }
}

fn report(cx: &mut Ctx, span: Span, t: TyId, callee: DefId, iterating: bool) {
    let name = match &cx.defs[callee.0 as usize] {
        Some(Def::Fn(f)) => f.name.clone(),
        _ => String::new(),
    };
    let (from, map) = container(&name);
    // `T | null` from a read that may miss: name the value.
    let t = cx.ty.opt_payload(t).unwrap_or(t);
    let tn = cx.display(t);
    let msg = if iterating {
        let what = from.trim_start_matches(" out of ");
        format!("iterating {what} would copy a `{tn}`")
    } else {
        format!("{} would copy a `{tn}`{from}", written(&name))
    };
    let direct = matches!(cx.ty.kind(t), TyKind::Promise(..));
    let note = match (direct, map) {
        (false, true) => format!(
            "{WHY}, nor can a value type that holds one: store the awaited result instead of the \
             promise, or make `{tn}` a class (a class instance is shared, not copied)"
        ),
        (false, false) => format!(
            "{WHY}, nor can a value type that holds one: read its fields in place \
             (`xs[i].field`), take it out with `pop()` or `splice(i, 1)`, or make `{tn}` a class \
             (a class instance is shared, not copied)"
        ),
        (true, true) => format!(
            "{WHY}: store the awaited result (`m.set(k, await p)`), or keep the promises in an \
             array and take them out with `pop()` or `splice(i, 1)`"
        ),
        (true, false) => format!(
            "{WHY}: take promises out with `pop()` or `splice(i, 1)`, or await them together \
             with `Promise.all(arr)`"
        ),
    };
    cx.error(Diagnostic::error(msg, span).with_note(note));
}

/// Does sharing a `t` (a copy of values in place, a new reference to counted objects) copy a
/// promise?
pub(crate) fn share_copies_promise(cx: &mut Ctx, t: TyId) -> bool {
    copies_promise(cx, t, false, &mut vec![])
}
