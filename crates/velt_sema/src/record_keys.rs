//! Key types of `Record<K, V>` (docs/internals/design/record.md): `string` (an *open* record), a
//! union of string literal types or a string enum (a *closed* record, which has every key).
//!
//! A written `Record<K, V>` with a concrete `K` is checked where the type is resolved
//! (`resolve.rs`). A key that comes from a type parameter is checked where the parameter meets a
//! concrete type: after all bodies, [`check_instantiations`] collects the key types each
//! function's types mention (through struct, class and union fields too), propagates the generic
//! ones to every caller (and from a closure to its enclosing function) like the JSON check, and
//! to every function that mentions a class type whose methods are dispatched dynamically
//! ([`crate::dispatch`]), revisiting only the functions whose callees' keys grew
//! ([`crate::dispatch::Rounds`]), and reports the concrete ones that are not keys. Lowering never
//! sees a record with another key.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, instantiate, Dispatch, Pass, Rounds};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, LitValue, TyId, TyKind};
use crate::types::{children, collect_params};
use crate::visit;

const KEY_NOTE: &str =
    "record keys are `string`, a union of string literals or a string enum; use `Map<K, V>` for other keys";

impl Ctx<'_> {
    /// `(K, V)` if `t` is the prelude's `Record<K, V>`.
    pub(crate) fn record_type_args(&self, t: TyId) -> Option<(TyId, TyId)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if Some(*d) == self.prelude_adt("Record") => match args[..] {
                [k, v] => Some((k, v)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The keys of a closed record (`None`: `K` is `string`, a type parameter or not a key).
    pub(crate) fn record_key_names(&mut self, k: TyId) -> Option<Vec<String>> {
        if let Some(ms) = self.union_members(k) {
            return ms
                .into_iter()
                .map(|m| match self.ty.kind(m) {
                    TyKind::Literal(LitValue::Str(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
        }
        match self.ty.kind(k) {
            TyKind::Literal(LitValue::Str(s)) => Some(vec![s.clone()]),
            TyKind::Adt(d, _) => {
                let e = self.enum_info(*d)?;
                e.variants.iter().map(|v| v.str_value.clone()).collect()
            }
            _ => None,
        }
    }

    /// Does `k` mention a type parameter (a key only known per instantiation)?
    pub(crate) fn is_generic_key(&self, k: TyId) -> bool {
        let mut ps = vec![];
        collect_params(&self.ty, k, &mut ps);
        !ps.is_empty()
    }

    /// Is `k` a valid record key type? Reports at `span` if not (`why`: an extra note saying
    /// where the key type comes from). Type parameters are checked per instantiation.
    pub(crate) fn check_record_key(&mut self, k: TyId, span: Span, why: Option<String>) -> bool {
        let d = if let TyKind::Literal(LitValue::Str(s)) = self.ty.kind(k) {
            Diagnostic::error(
                format!("a `Record` with the single key \"{s}\" is not supported"),
                span,
            )
            .with_note(format!("use an object type: `{{ {s}: V }}`"))
        } else if matches!(self.ty.kind(k), TyKind::Str | TyKind::Error)
            || self.is_generic_key(k)
            || self.record_key_names(k).is_some()
        {
            return true;
        } else {
            let kn = self.display(k);
            Diagnostic::error(format!("`{kn}` cannot be a `Record` key"), span).with_note(KEY_NOTE)
        };
        self.error(match why {
            Some(w) => d.with_note(w),
            None => d,
        });
        false
    }
}

/// Record-key facts of one function body.
#[derive(Default)]
struct Uses {
    /// Every type the body mentions (expressions, locals and type arguments of calls), with its
    /// first span.
    types: Vec<(TyId, Span)>,
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    closures: Vec<DefId>,
    /// The methods the class types among `types` dispatch dynamically to, with those types'
    /// arguments and the span of the type.
    dyn_calls: Vec<(DefId, Vec<TyId>, Span)>,
}

impl Uses {
    /// The defs whose requirements this function's requirements are made of.
    fn reads(&self) -> impl Iterator<Item = DefId> + '_ {
        let calls = self.calls.iter().map(|(d, _, _)| *d);
        let dyn_calls = self.dyn_calls.iter().map(|(d, _, _)| *d);
        calls.chain(self.closures.iter().copied()).chain(dyn_calls)
    }

    /// The work of one visit, for [`crate::dispatch::instantiation_work`].
    fn size(&self) -> u64 {
        (self.types.len() + self.calls.len() + self.closures.len() + self.dyn_calls.len()) as u64
    }
}

/// A key type a function needs to be valid, and the function whose types mention it.
type Need = (TyId, DefId);

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let mut uses = collect(cx);
    let mut memo: HashMap<TyId, Vec<TyId>> = HashMap::new();
    let mut dispatch = Dispatch::default();
    for (_, u) in &mut uses {
        for (t, span) in &u.types {
            for (m, args) in dispatch.targets(cx, *t) {
                u.dyn_calls.push((m, args, *span));
            }
        }
    }
    let reads = uses
        .iter()
        .enumerate()
        .flat_map(|(i, (_, u))| u.reads().map(move |d| (i, d)));
    let mut rounds = Rounds::new(uses.len(), reads);
    let mut work = dispatch.work;
    let mut needs: HashMap<DefId, Vec<Need>> = HashMap::new();
    // Concrete keys per (function, key): the span and origin to report a bad one with.
    let mut concrete: Vec<((DefId, TyId), Span, DefId)> = vec![];
    let mut seen: HashSet<(DefId, TyId, DefId)> = HashSet::new();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        work += u.size();
        let mut grew = false;
        for (k, span, origin) in requirements(cx, *f, u, &needs, &mut memo) {
            if cx.is_generic_key(k) {
                let n = needs.entry(*f).or_default();
                if !n.contains(&(k, origin)) {
                    n.push((k, origin));
                    grew = true;
                }
            } else if seen.insert((*f, k, origin)) {
                concrete.push(((*f, k), span, origin));
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
    add_work(Pass::RecordKeys, work);
    report(cx, &concrete);
}

/// The key types function `f` needs: those its own types mention, and those of the generic
/// functions it calls (substituted), closures it creates and methods the class types it
/// mentions dispatch dynamically to (instantiated with those types' arguments), with the span
/// to report them at.
fn requirements(
    cx: &mut Ctx,
    f: DefId,
    u: &Uses,
    needs: &HashMap<DefId, Vec<Need>>,
    memo: &mut HashMap<TyId, Vec<TyId>>,
) -> Vec<(TyId, Span, DefId)> {
    let mut reqs = vec![];
    for (d, targs, span) in &u.calls {
        for (k, origin) in needs.get(d).into_iter().flatten() {
            reqs.push((cx.ty.subst(*k, targs), *span, *origin));
        }
    }
    for c in &u.closures {
        for (k, origin) in needs.get(c).into_iter().flatten() {
            reqs.push((*k, cx.def_spans[c.0 as usize], *origin));
        }
    }
    for (t, span) in &u.types {
        for k in keys_in(cx, *t, memo, &mut vec![]) {
            reqs.push((k, *span, f));
        }
    }
    for (m, args, span) in &u.dyn_calls {
        for (k, origin) in needs.get(m).into_iter().flatten() {
            if let Some(k) = instantiate(cx, *k, args) {
                reqs.push((k, *span, *origin));
            }
        }
    }
    reqs
}

/// Report each bad key once per function, preferably where a generic function brought it in.
fn report(cx: &mut Ctx, concrete: &[((DefId, TyId), Span, DefId)]) {
    // The first entry per (function, key) that a generic function brought in.
    let mut brought: HashMap<(DefId, TyId), (Span, DefId)> = HashMap::new();
    for (fk, span, origin) in concrete {
        if *origin != fk.0 {
            brought.entry(*fk).or_insert((*span, *origin));
        }
    }
    let mut done: HashSet<(DefId, TyId)> = HashSet::new();
    let mut picked = vec![];
    for (fk, span, origin) in concrete {
        if done.insert(*fk) {
            let at = brought.get(fk).copied().unwrap_or((*span, *origin));
            picked.push((*fk, at));
        }
    }
    picked.sort_by_key(|(_, (s, _))| (s.file, s.lo));
    for ((f, k), (span, origin)) in picked {
        let why = origin_note(cx, origin, f);
        cx.check_record_key(k, span, why);
    }
}

/// Where a key that failed at a call comes from: the generic function that uses it.
fn origin_note(cx: &Ctx, origin: DefId, here: DefId) -> Option<String> {
    let Some(Def::Fn(f)) = &cx.defs[origin.0 as usize] else {
        return None;
    };
    let std = f.name.starts_with("std/");
    (origin != here && !std)
        .then(|| format!("required because `{}` uses it as a `Record` key", f.name))
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Uses)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut u = Uses::default();
        let mut seen: HashSet<TyId> = HashSet::new();
        for l in &f.body.locals {
            if seen.insert(l.ty) {
                u.types.push((l.ty, l.span));
            }
        }
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if seen.insert(e.ty) {
                u.types.push((e.ty, e.span));
            }
            match &e.kind {
                E::Call {
                    callee: Callee::Def(d, targs),
                    ..
                } if !targs.is_empty() => {
                    for t in targs {
                        if seen.insert(*t) {
                            u.types.push((*t, e.span));
                        }
                    }
                    u.calls.push((*d, targs.clone(), e.span));
                }
                E::Closure(c) => u.closures.push(*c),
                _ => {}
            }
        });
        out.push((DefId(i as u32), u));
    }
    out
}

/// The record key types `t` contains, also through the fields of structs, classes and unions
/// (`stack`: types being visited, so recursive types terminate).
fn keys_in(
    cx: &mut Ctx,
    t: TyId,
    memo: &mut HashMap<TyId, Vec<TyId>>,
    stack: &mut Vec<TyId>,
) -> Vec<TyId> {
    if let Some(ks) = memo.get(&t) {
        return ks.clone();
    }
    if stack.contains(&t) {
        return vec![];
    }
    stack.push(t);
    let kind = cx.ty.kind(t).clone();
    let mut parts = children(&kind);
    let mut out = vec![];
    if let Some((k, _)) = cx.record_type_args(t) {
        out.push(k);
    } else if let TyKind::Adt(d, args) = &kind {
        for f in member_types(cx, *d) {
            parts.push(cx.ty.subst(f, args));
        }
    }
    for p in parts {
        for k in keys_in(cx, p, memo, stack) {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    stack.pop();
    // Inner results may be cut short by the recursion guard: keep only complete ones.
    if stack.is_empty() {
        memo.insert(t, out.clone());
    }
    out
}

/// Field types of a struct, class or object type; payload types of an enum (generic).
fn member_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
        DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}
