//! What `JSON.stringify` / `JSON.parse<T>` can be generated for (after all bodies): numbers,
//! `bool`, `string`, arrays, `T | null`, C-like enums, unions (stringify only), structs / classes
//! / object literals whose fields are all public and serializable, and the prelude's `JsonValue`.
//! `JSON.stringify` writes `private` fields (as Node does) and skips ES private fields (`#x`);
//! decoding cannot set either. A type holding a std type's private field has no JSON form at
//! all: std types keep runtime handles (pointers) there, and decoding one from untrusted input
//! would forge it. A class with a private
//! or protected constructor can be written but not decoded: decoding fills the fields without
//! running a constructor, which would bypass the class's factories and their checks. The intrinsics sit in generic prelude code
//! (`JSON.stringify<T>`), so a requirement on a type parameter propagates to every caller (and
//! from a closure to its enclosing function) until it meets a concrete type, which is checked at
//! that call site. A method dispatched dynamically (through an interface or a base class) has
//! no call with type arguments: its requirements are instantiated wherever its class type is
//! mentioned ([`crate::dispatch`]). The fixed point revisits only the functions whose callees'
//! requirements grew ([`crate::dispatch::Rounds`]).
//!
//! The same propagation carries how generic code looks at its values for
//! [`crate::object_copies`]: `console.log` and `===` on a type parameter (std's `indexOf` and
//! `includes`, a user's `eq<T>`) look at whatever type each call instantiates it with.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, instantiate, Dispatch, Pass, Rounds};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::inst_sites::{carry_guard, guards_of};
use crate::object_copies::Seen;
use crate::types::children;
use crate::visit;

pub(crate) mod handles;
mod identity;
mod uses;

use uses::{uses_of, PARSE_TOP};

/// What a function needs of a type: a JSON form (`true`: decoded by `JSON.parse`), or a look
/// at its values ([`crate::object_copies`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Need {
    Json(bool),
    Seen(Seen),
    /// Read without a lock (printed, formatted, converted to `any`): no `Mutex` or `RwLock`
    /// may be in it. Recorded only for a type parameter; concrete types are checked where
    /// they are read (`Ctx::holds_lock`).
    NoLock,
}

/// JSON-relevant facts of one function body.
#[derive(Default)]
struct Uses {
    /// Types given to the JSON intrinsics, `console.log` and `===` directly.
    direct: Vec<(TyId, Span, Need)>,
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    /// The guard bits of each of `calls` (`inst_sites::carry_guard`).
    guards: Vec<u64>,
    closures: Vec<DefId>,
    /// Every type the body mentions (expressions and type arguments of calls), with its first
    /// span: the methods of class types among them may be dispatched dynamically.
    types: Vec<(TyId, Span)>,
    /// Those methods, with their class type's arguments and the span of the type.
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
        (self.direct.len() + self.calls.len() + self.closures.len() + self.dyn_calls.len()) as u64
    }
}

/// Types (and whether `JSON.parse` decodes them) a function needs to have a JSON form, with
/// their guard bits (`inst_sites::carry_guard`).
type Needs = FxHashMap<DefId, Vec<(TyId, Need, u64)>>;

pub(crate) fn check_json_types(cx: &mut Ctx) {
    let mut uses = collect(cx);
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
    let std_fns: FxHashSet<DefId> = uses
        .iter()
        .map(|(f, _)| *f)
        .filter(|f| is_std_fn(cx, *f))
        .collect();
    let mut rounds = Rounds::new(uses.len(), reads);
    let mut work = dispatch.work + uses.iter().map(|(_, u)| u.types.len() as u64).sum::<u64>();
    let mut needs: Needs = FxHashMap::default();
    let mut checked: FxHashSet<(TyId, Span, Need)> = FxHashSet::default();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        work += u.size();
        let mut grew = false;
        for (t, span, parse, guard) in requirements(cx, u, &needs) {
            if has_param(cx, t) {
                let n = needs.entry(*f).or_default();
                if !n.contains(&(t, parse, guard)) {
                    n.push((t, parse, guard));
                    grew = true;
                }
            } else if let Need::Seen(how) = parse {
                // The standard library's own looks at its concrete types (`any` in its proxy
                // code) never see a user's object types, which reach it only as type
                // arguments, nor whether the user's code runs them.
                if !std_fns.contains(f) && checked.insert((t, span, parse)) {
                    cx.object_copies.observe(t, span, how);
                }
            } else if parse == Need::NoLock {
                if checked.insert((t, span, parse)) && cx.holds_lock(t) {
                    let tn = cx.display(t);
                    cx.error(
                        Diagnostic::error(
                            format!("`{tn}` is read here without its lock (printed, formatted or converted to `any` through a type parameter)"),
                            span,
                        )
                        .with_note("a value holding a lock is read only under it (`with`): pass what a callback returns instead"),
                    );
                }
            } else if let Need::Json(_) = parse {
                // A type `JSON.parse` decodes: an object type's layout its extensions widen
                // (`collect::extended_layouts`). What it cannot decode is reported by its
                // generator (velt/encoding/json/generate/parse_check.vlt).
                if !cx.objects.layouts.families.is_empty() {
                    cx.objects.layouts.parsed.push(t);
                }
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
    add_work(Pass::Json, work);
}

/// The types a function needs: those it gives the JSON intrinsics, those of the generic
/// functions it calls (substituted), closures it creates and methods the class types it
/// mentions dispatch dynamically to (instantiated with those types' arguments).
fn requirements(cx: &mut Ctx, u: &Uses, needs: &Needs) -> Vec<(TyId, Span, Need, u64)> {
    let mut reqs: Vec<(TyId, Span, Need, u64)> =
        u.direct.iter().map(|(t, s, n)| (*t, *s, *n, 0)).collect();
    for ((d, targs, span), site) in u.calls.iter().zip(&u.guards) {
        for (t, parse, need) in needs.get(d).into_iter().flatten() {
            if let Some(g) = carry_guard(cx, *need, targs, *site) {
                reqs.push((cx.subst(*t, targs), *span, *parse, g));
            }
        }
    }
    for c in &u.closures {
        for (t, parse, need) in needs.get(c).into_iter().flatten() {
            reqs.push((*t, cx.def_spans[c.0 as usize], *parse, *need));
        }
    }
    for (m, args, span) in &u.dyn_calls {
        for (t, parse, need) in needs.get(m).into_iter().flatten() {
            let Some(g) = carry_guard(cx, *need, args, 0) else {
                continue;
            };
            if let Some(t) = instantiate(cx, *t, args) {
                reqs.push((t, *span, *parse, g));
            }
        }
    }
    reqs
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Uses)> {
    let mut out = vec![];
    // Calls of generated functions look at their type arguments' shapes (`generated_look`).
    let generated: FxHashSet<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.generator_of(*d).is_some())
        .collect();
    // `JSON.parse<T>` calls the generated `parseJsonTop<T>` (velt/encoding/json), whose code
    // decodes the types its generator ports and calls `jsonParse<T>` for the others: the
    // calls of the generated function need `T` decoded, so a type without a JSON form is
    // reported where the program parses it, never in the generated code (step 5 of
    // docs/internals/design/source-generators.md moves these checks into the generator).
    let parsers: FxHashSet<DefId> = generated
        .iter()
        .copied()
        .filter(|d| matches!(&cx.info[d.0 as usize], DefInfo::Fn(f) if f.name.ends_with(PARSE_TOP)))
        .collect();
    // Only a generated function that is given values of its type arguments can look at them
    // (`writeJson<T>(w, value: T)`); one that makes them (`parseJsonTop<T>(text): T`) cannot.
    let lookers: FxHashSet<DefId> = generated
        .iter()
        .copied()
        .filter(|d| match &cx.info[d.0 as usize] {
            DefInfo::Fn(f) => f.params.iter().any(|p| has_param(cx, p.ty)),
            _ => false,
        })
        .collect();
    let generated_modules: FxHashSet<usize> = (0..cx.modules.len())
        .filter(|m| cx.is_generated_module(*m))
        .collect();
    // Looks at values matter only for object copies; most programs make none.
    let looks = !cx.object_copies.copies.is_empty();
    let copies: Vec<(Span, TyId)> = cx
        .object_copies
        .copies
        .iter()
        .map(|c| (c.span, c.to))
        .collect();
    let mut ids = identity::Looks {
        any_impl: cx.any.and_then(|a| a.imp),
        proxy: cx.prelude_adt("Proxy"),
        tys: &cx.ty,
        copies: &copies,
        f: DefId(0),
        first: cx.object_copies.dyn_sources.len(),
        sources: vec![],
    };
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let in_generated = matches!(
            cx.info.get(i),
            Some(DefInfo::Fn(info)) if generated_modules.contains(&info.module)
        );
        let mut u = Uses::default();
        let mut seen: FxHashSet<TyId> = FxHashSet::default();
        ids.f = DefId(i as u32);
        let guards = guards_of(ids.tys, &mut f.body.block);
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            let calls = u.calls.len();
            if seen.insert(e.ty) {
                u.types.push((e.ty, e.span));
            }
            // Type arguments of generic calls and of generic functions used as values.
            let targs = match &e.kind {
                E::Call {
                    callee: Callee::Def(_, targs),
                    ..
                }
                | E::FnRef(_, targs) => targs.as_slice(),
                _ => &[],
            };
            for t in targs {
                if seen.insert(*t) {
                    u.types.push((*t, e.span));
                }
            }
            uses_of(ids.tys, &mut u, e, looks, (&parsers, in_generated));
            if looks {
                identity::generated_look(&lookers, e, &mut u.direct);
            }
            if u.calls.len() > calls {
                let guard = guards.get(&(e as *const Expr)).copied().unwrap_or(0);
                u.guards.push(guard);
            }
            if looks {
                ids.uses(&mut u.direct, e);
            }
        });
        out.push((DefId(i as u32), u));
    }
    let sources = std::mem::take(&mut ids.sources);
    cx.object_copies.dyn_sources.extend(sources);
    out
}

/// Is `f` a function (or closure) of the standard library?
fn is_std_fn(cx: &Ctx, f: DefId) -> bool {
    let Some(DefInfo::Fn(info)) = cx.info.get(f.0 as usize) else {
        return false;
    };
    cx.modules.get(info.module).is_some_and(|m| m.is_std)
}

/// [`has_param`] with the types at hand.
fn param_in(tys: &crate::types::Types, t: TyId) -> bool {
    match tys.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| param_in(tys, c)),
    }
}

fn has_param(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| has_param(cx, c)),
    }
}

impl Ctx<'_> {
    /// Is type definition `d` declared in a std module?
    pub(crate) fn declared_in_std(&self, d: DefId) -> bool {
        self.adt(d).is_some_and(|a| self.scopes[a.module].is_std)
    }
}
