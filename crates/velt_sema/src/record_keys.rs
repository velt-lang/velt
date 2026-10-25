//! Key types of `Record<K, V>` (docs/internals/design/record.md): `string` (an *open* record), a
//! union of string literal types or a string enum (a *closed* record, which has every key).
//!
//! A written `Record<K, V>` with a concrete `K` is checked where the type is resolved
//! (`resolve.rs`); there `number` and `string | number` keys become `string`, as JavaScript
//! makes a number key a string ([`Ctx::number_key_as_string`]). A key that comes from a type parameter is checked where the parameter meets a
//! concrete type: after all bodies, [`check_instantiations`] collects the key types each function's
//! types mention (through struct, class and union fields too), propagates the generic ones to every
//! instantiation site (calls, function values, `new` and generic methods called through a type
//! parameter: [`crate::inst_sites`]) and from a closure to its enclosing function like the JSON
//! check, and to every function that mentions a class type whose methods are dispatched dynamically
//! ([`crate::dispatch`]), revisiting only the functions whose callees' keys grew
//! ([`crate::dispatch::Rounds`]), and reports the concrete ones that are not keys. Lowering never
//! sees a record with another key.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, instantiate, Dispatch, Pass, Rounds};
use crate::hir::{Def, DefId, LitValue, TyId, TyKind};
use crate::inst_sites::{self, carry_guard, resolve_param_calls, ParamCall, Uses};
use crate::types::collect_params;

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

    /// The key type of a written `Record<k, V>`: `number` and `string | number` are `string`
    /// (Velt's integer types stay errors).
    /// JavaScript converts a number key to its string, so such a record holds string keys
    /// (`Object.keys` lists them, in its order: array indices first) and a number index is
    /// converted where it is used (`r[1]` is `r["1"]`, `body::expr::record_call`).
    pub(crate) fn number_key_as_string(&mut self, k: TyId) -> TyId {
        let str_ = self.ty.str_;
        let num = self.ty.f64;
        if k == num {
            return str_;
        }
        match self.union_members(k) {
            Some(ms) if ms.iter().all(|m| *m == str_ || *m == num) => str_,
            _ => k,
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

/// A key type a function needs to be valid, the function whose types mention it and its guard
/// bits (`inst_sites::carry_guard`).
type Need = (TyId, DefId, u64);

/// A key type a function needs, the span to report it at, the function whose types mention it
/// and its guard bits.
type Req = (TyId, Span, DefId, u64);

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let mut uses = inst_sites::collect(cx, false);
    let mut memo: FxHashMap<TyId, Vec<TyId>> = FxHashMap::default();
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
    let mut needs: FxHashMap<DefId, Vec<Need>> = FxHashMap::default();
    // The param calls whose receiver is generic in each function.
    let mut pneeds: FxHashMap<DefId, Vec<ParamCall>> = FxHashMap::default();
    // Concrete keys per (function, key): the span and origin to report a bad one with.
    let mut concrete: Vec<((DefId, TyId), Span, DefId)> = vec![];
    let mut seen: FxHashSet<(DefId, TyId, DefId)> = FxHashSet::default();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        work += u.size();
        let mut grew = false;
        let (mut reqs, pending) = requirements(cx, *f, u, &needs, &pneeds, &mut memo);
        for (p, span) in pending {
            let generic = resolve_param_calls(cx, &mut rounds, i, vec![p], &mut |cx, m, args| {
                for (k, origin, _) in needs.get(&m).into_iter().flatten() {
                    reqs.push((cx.subst(*k, args), span, *origin, 0));
                }
                let ps = pneeds.get(&m).into_iter().flatten();
                ps.map(|p| p.subst(cx, args)).collect()
            });
            for p in generic {
                let n = pneeds.entry(*f).or_default();
                if !n.contains(&p) {
                    n.push(p);
                    grew = true;
                }
            }
        }
        for (k, span, origin, guard) in reqs {
            if cx.is_generic_key(k) {
                let n = needs.entry(*f).or_default();
                if !n.contains(&(k, origin, guard)) {
                    n.push((k, origin, guard));
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
/// to report them at; and the param calls that reach it (its own, and those of its callees,
/// closures and dynamically dispatched methods), with the span to report what they bring in at.
fn requirements(
    cx: &mut Ctx,
    f: DefId,
    u: &Uses,
    needs: &FxHashMap<DefId, Vec<Need>>,
    pneeds: &FxHashMap<DefId, Vec<ParamCall>>,
    memo: &mut FxHashMap<TyId, Vec<TyId>>,
) -> (Vec<Req>, Vec<(ParamCall, Span)>) {
    let mut reqs = vec![];
    let mut pending = u.param_calls.clone();
    for ((d, targs, span), site) in u.calls.iter().zip(&u.guards) {
        for (k, origin, need) in needs.get(d).into_iter().flatten() {
            if let Some(g) = carry_guard(cx, *need, targs, *site) {
                reqs.push((cx.subst(*k, targs), *span, *origin, g));
            }
        }
        for p in pneeds.get(d).into_iter().flatten() {
            pending.push((p.subst(cx, targs), *span));
        }
    }
    for c in &u.closures {
        let span = cx.def_spans[c.0 as usize];
        for (k, origin, need) in needs.get(c).into_iter().flatten() {
            reqs.push((*k, span, *origin, *need));
        }
        for p in pneeds.get(c).into_iter().flatten() {
            pending.push((p.clone(), span));
        }
    }
    for (t, span) in &u.types {
        for k in crate::key_groups::keys_in(cx, *t, memo) {
            reqs.push((k, *span, f, 0));
        }
    }
    for (m, args, span) in &u.dyn_calls {
        for (k, origin, need) in needs.get(m).into_iter().flatten() {
            let Some(g) = carry_guard(cx, *need, args, 0) else {
                continue;
            };
            if let Some(k) = instantiate(cx, *k, args) {
                reqs.push((k, *span, *origin, g));
            }
        }
        for p in pneeds.get(m).into_iter().flatten() {
            pending.extend(p.instantiate(cx, args).map(|p| (p, *span)));
        }
    }
    (reqs, pending)
}

/// Report each bad key once per function, preferably where a generic function brought it in.
fn report(cx: &mut Ctx, concrete: &[((DefId, TyId), Span, DefId)]) {
    // The first entry per (function, key) that a generic function brought in.
    let mut brought: FxHashMap<(DefId, TyId), (Span, DefId)> = FxHashMap::default();
    for (fk, span, origin) in concrete {
        if *origin != fk.0 {
            brought.entry(*fk).or_insert((*span, *origin));
        }
    }
    let mut done: FxHashSet<(DefId, TyId)> = FxHashSet::default();
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

/// Where a key that failed at a call comes from: the generic function that uses it, or the
/// class or struct whose field initializers do (instantiated by `new`).
fn origin_note(cx: &Ctx, origin: DefId, here: DefId) -> Option<String> {
    let name = match (&cx.defs[origin.0 as usize], &cx.info[origin.0 as usize]) {
        (Some(Def::Fn(f)), _) => f.name.as_str(),
        (_, DefInfo::Adt(a)) => a.name.as_str(),
        _ => return None,
    };
    let std = name.starts_with("std/");
    (origin != here && !std).then(|| format!("required because `{name}` uses it as a `Record` key"))
}
