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
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{add_work, instantiate, Dispatch, Pass, Rounds};
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::inst_sites::{carry_guard, guards_of};
use crate::object_copies::Seen;
use crate::types::children;
use crate::visit;

mod handles;
mod identity;
mod union;
mod uses;

use union::union_decode_problem;
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
    // A type without a JSON form is reported once per place, whether it is parsed, written or
    // both (`JSON.stringify(JSON.parse<T>(s))`).
    let mut reported: FxHashSet<(TyId, Span)> = FxHashSet::default();
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
            } else if let Need::Json(parse) = parse {
                if parse && !cx.objects.layouts.families.is_empty() {
                    cx.objects.layouts.parsed.push(t);
                }
                if !reported.contains(&(t, span))
                    && checked.insert((t, span, Need::Json(parse)))
                    && check(cx, t, span, parse)
                {
                    reported.insert((t, span));
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

/// `JSON.parse` into, or `JSON.stringify` of, a dynamic type `name` (in `tn` unless `whole`):
/// not supported yet (#924).
fn report_dynamic(cx: &mut Ctx, name: &str, whole: bool, tn: &str, span: Span, parse: bool) {
    let within = match whole {
        true => String::new(),
        false => format!(" (in `{tn}`)"),
    };
    let d = match parse {
        true => Diagnostic::error(
            format!("`JSON.parse` into `{name}`{within} is not supported yet (#924)"),
            span,
        )
        .with_note("TypeScript gives an `any` holding whatever the text has; Velt decodes into a type and checks the data against it")
        .with_note("name the type the JSON must have: `JSON.parse(text) as User`, `JSON.parse<User>(text)` or `const u: User = JSON.parse(text)`, or use `JSON.parseValue(text)` (a `JsonValue`) for data of unknown shape"),
        false => Diagnostic::error(
            format!("`JSON.stringify` of `{name}`{within} is not supported yet (#924)"),
            span,
        )
        .with_note("convert the value to its type first: `JSON.stringify(x as User)` (checked at run time)"),
    };
    cx.error(d);
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

/// Reports `t` at `span` if it has no JSON form (`true`: reported).
fn check(cx: &mut Ctx, t: TyId, span: Span, parse: bool) -> bool {
    // A class instance brand (`crate::brands`) is written and read as its class.
    let t = match cx.brand_base(t) {
        Some(b) if cx.brand_operand(b) && !cx.brandable(b) => b,
        _ => t,
    };
    if !parse {
        let seen = crate::object_copies::Seen::Serialized;
        cx.object_copies.observe(t, span, seen);
    }
    if let Some(what) = undefined_problem(cx, t, parse) {
        cx.error(Diagnostic::error(
            format!("{what} is not supported yet"),
            span,
        ));
        return true;
    }
    let mut stack = vec![];
    let Some(bad) = unserializable(cx, t, &mut stack, parse) else {
        return false;
    };
    let (tn, bn) = (cx.display(t), cx.display(bad));
    if let Some(why) = cx
        .union_def(bad)
        .and_then(|_| union_decode_problem(cx, bad))
    {
        let within = if t == bad {
            String::new()
        } else {
            format!(" (in `{tn}`)")
        };
        let fix = if why.contains("objects") {
            "give each object member a literal field such as `kind: \"a\"` (a discriminant), or parse a `JsonValue` and build the union from it"
        } else if why.contains("are both the JSON") {
            "give each member its own values, or parse a `JsonValue` and build the union from it"
        } else {
            "parse a `JsonValue` and build the union from it"
        };
        cx.error(
            Diagnostic::error(
                format!("`JSON.parse` cannot decode the union `{bn}`{within}: {why}"),
                span,
            )
            .with_note(fix),
        );
        return true;
    }
    if parse && report_restricted_ctor(cx, t, bad, span) {
        return true;
    }
    if matches!(cx.ty.kind(bad), TyKind::Adt(d, _) if cx.is_generator_class(*d)) {
        let what = match t == bad {
            true => format!("`{tn}` is a generator"),
            false => format!("`{tn}` contains the generator `{bn}`"),
        };
        cx.error(
            Diagnostic::error(format!("cannot convert to or from JSON: {what}"), span).with_note(
                "TypeScript allows this (`JSON.stringify` writes a generator as `{}`); Velt doesn't because a generator is a paused computation, not data; write the values it yields instead (collect them into an array with `for...of`)",
            ),
        );
        return true;
    }
    if let Some(d) = cx.dynamic(bad) {
        report_dynamic(cx, d.name(), t == bad, &tn, span, parse);
        return true;
    }
    let private = private_field(cx, bad, parse);
    // Decoding cannot set private fields; writing is refused only for std's types (handles).
    let what = match (&private, t == bad, parse) {
        (Some((field, _)), true, true) => {
            format!("`{tn}` has a private field `{field}`, which decoding cannot set")
        }
        (Some((field, _)), false, true) => {
            format!("`{tn}` contains `{bn}`, whose private field `{field}` decoding cannot set")
        }
        (Some((field, _)), true, false) => {
            format!("`{tn}` has a private field `{field}`, so it has no JSON form")
        }
        (Some((field, _)), false, false) => format!(
            "`{tn}` contains `{bn}`, which has a private field `{field}`, so it has no JSON form"
        ),
        (None, true, _) => format!("`{tn}` has no JSON form"),
        (None, false, _) => format!("`{tn}` contains `{bn}`, which has no JSON form"),
    };
    let mut d = Diagnostic::error(format!("cannot convert to or from JSON: {what}"), span);
    d = match private {
        Some((_, field_span)) if parse => d.with_label(field_span, "private field").with_note(
            "decoding fills fields without running the constructor, so it cannot build a type with private fields; decode into a type with public fields and construct it from that",
        ),
        Some((_, field_span)) => d.with_label(field_span, "private field").with_note(
            "a type of std or of a native library keeps its handles in private fields, so it has no JSON form; convert it to a type with public fields first",
        ),
        None => d.with_note(
            "JSON supports numbers, boolean, string, literal types, enums, arrays, tuples, `T | null`, `Map<string, T>`, structs, classes and object literals of those, and `JsonValue`",
        ),
    };
    if matches!(cx.ty.kind(bad), TyKind::Adt(d, _) if Some(*d) == cx.prelude_adt("Map")) {
        d = d.with_note("a `Map` converts to a JSON object only with `string` keys");
    }
    cx.error(d);
    true
}

/// Reports decoding `bad` (part of `t`) if its constructor is private or protected (`true`:
/// reported).
fn report_restricted_ctor(cx: &mut Ctx, t: TyId, bad: TyId, span: Span) -> bool {
    let TyKind::Adt(d, _) = cx.ty.kind(bad) else {
        return false;
    };
    let visibility = match cx.ctor_visibility(*d) {
        ast::CtorVisibility::Public => return false,
        ast::CtorVisibility::Private => "private",
        ast::CtorVisibility::Protected => "protected",
    };
    let name = cx.adt(*d).map(|a| a.name.clone()).unwrap_or_default();
    let within = if t == bad {
        String::new()
    } else {
        format!(" (in `{}`)", cx.display(t))
    };
    let article = if name.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    };
    cx.error(
        Diagnostic::error(
            format!("`JSON.parse` cannot create {article} `{name}`{within}: its constructor is {visibility}"),
            span,
        )
        .with_note(match cx.factories(*d) {
            Some(f) => format!("decoding fills the fields without running a constructor; decode into a plain object type and call {f}"),
            None => format!("decoding fills the fields without running a constructor; decode into a plain object type and create the `{name}` from it with a static factory method"),
        }),
    );
    true
}

/// The first private field of struct or class type `t` (name and declaration) that rules out
/// JSON (`parse`: any, else one a std type declares), if it has one.
fn private_field(cx: &Ctx, t: TyId, parse: bool) -> Option<(String, Span)> {
    let TyKind::Adt(d, _) = cx.ty.kind(t) else {
        return None;
    };
    let d = *d;
    // The prelude's `Map` and `Record` have their own rules (and private fields of their own).
    if Some(d) == cx.prelude_adt("Map") || Some(d) == cx.prelude_adt("Record") {
        return None;
    }
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a
            .fields
            .iter()
            .find(|f| {
                f.private_to.is_some_and(|o| parse || cx.keeps_handles(o))
                    && !cx.plain_private(f.private_to, &f.name)
            })
            .map(|f| (f.name.clone(), f.span)),
        _ => None,
    }
}

/// What this phase of `undefined` (issue #930) cannot convert yet: `JSON.parse` into a type
/// that holds a `T | undefined` anywhere (a `T | null | undefined` reads as its `T | null`), and `JSON.stringify` of a value that may be
/// `undefined` itself (Node returns `undefined`, not a string). Object fields and `Map` /
/// `Record` values holding `undefined` (left out) and array elements (`null`) are written as
/// Node does.
/// A class in the value whose `toJSON()` may return `undefined` is not: Node leaves such a
/// field out and writes `null` for such an array element, where the glue writes what the
/// method returned.
fn undefined_problem(cx: &mut Ctx, t: TyId, parse: bool) -> Option<&'static str> {
    let undef = |cx: &Ctx, t: TyId| cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined());
    if parse {
        let mut stack = vec![t];
        let mut seen = FxHashSet::default();
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            // A `T | null | undefined` decodes into its `T | null` (JSON has no `undefined`).
            let three = cx.ty.nothing_of(t) == Some(crate::hir::Nothing::Both);
            if undef(cx, t) && !three {
                return Some("`JSON.parse` into a type that may hold `undefined`");
            }
            if cx.is_callable_object(t) || cx.ctor_sig_own(t).is_some() {
                return Some("`JSON.parse` into a function with properties (#921)");
            }
            stack.extend(children(cx.ty.kind(t)));
            if let TyKind::Adt(d, _) = cx.ty.kind(t) {
                stack.extend(parsed_field_tys(cx, *d));
            }
        }
        return None;
    }
    if undef(cx, t) {
        return Some("`JSON.stringify` of a value that may be `undefined` (Node returns `undefined`, not a string)");
    }
    // A function with properties is a function to JSON: left out of objects, `null` in arrays
    // (lowering), and `undefined` itself.
    if cx.is_callable_object(cx.ty.opt_payload(t).unwrap_or(t)) {
        return Some("`JSON.stringify` of a function with properties (Node returns `undefined`, not a string)");
    }
    let mut stack = vec![t];
    let mut seen = FxHashSet::default();
    while let Some(t) = stack.pop() {
        if !seen.insert(t) {
            continue;
        }
        if cx.ctor_sig_own(t).is_some() {
            return Some("`JSON.stringify` of a class used as a value (#943)");
        }
        if let TyKind::Adt(d, args) = cx.ty.kind(t).clone() {
            // A class with `toJSON()` is written as what the method returns, its own or a
            // subclass's (the value may be an instance of one).
            let mut to_json = false;
            let classes = match cx.writes_undefined {
                true => with_subclasses(cx, d, &args),
                false => vec![],
            };
            for (c, c_args) in classes {
                if let Some((_, ret)) = crate::hooks::hook(cx, c, &c_args, crate::hooks::TO_JSON) {
                    if undef(cx, ret) {
                        return Some(
                            "`JSON.stringify` of a value whose `toJSON()` may return `undefined` (Node leaves such a field out, and returns `undefined` for the value itself)",
                        );
                    }
                    to_json |= c == d;
                    stack.push(ret);
                }
            }
            if !to_json {
                stack.extend(raw_field_tys(cx, d));
            }
        }
        stack.extend(children(cx.ty.kind(t)));
    }
    None
}

/// Class `d<args>` and every class that extends it (each with its own type parameters as its
/// arguments); nothing for a type that is not a class.
fn with_subclasses(cx: &mut Ctx, d: DefId, args: &[TyId]) -> Vec<(DefId, Vec<TyId>)> {
    if cx.adt(d).is_none_or(|a| a.kind != AdtKind::Class) {
        return vec![];
    }
    let mut out = vec![(d, args.to_vec())];
    for s in (0..cx.info.len() as u32).map(DefId) {
        let Some(n) = cx
            .adt(s)
            .filter(|a| s != d && a.kind == AdtKind::Class)
            .map(|a| a.generics.len())
        else {
            continue;
        };
        if cx.class_extends(s, d) {
            let own = (0..n as u32).map(|i| cx.ty.param(i)).collect();
            out.push((s, own));
        }
    }
    out
}

/// The declared field types of struct / class `d` (and the payloads of enum `d`'s variants).
/// The field types `JSON.parse` decodes into: an optional field's declared type, whose own
/// `undefined` stands for an absent key (`a?: T` is a `T | undefined`).
fn parsed_field_tys(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Adt(a)) => a
            .fields
            .iter()
            .map(|f| if f.optional { f.declared } else { f.ty })
            .collect(),
        _ => raw_field_tys(cx, d),
    }
}

fn raw_field_tys(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Adt(a)) => a.fields.iter().map(|f| f.ty).collect(),
        Some(DefInfo::Enum(e)) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

impl Ctx<'_> {
    /// Is type definition `d` declared in a std module?
    pub(crate) fn declared_in_std(&self, d: DefId) -> bool {
        self.adt(d).is_some_and(|a| self.scopes[a.module].is_std)
    }
}

/// Lowering (`velt_vir` lower/json read.rs and write.rs) handles exactly what this accepts:
/// keep them in sync, or a type let through here is an internal error there.
/// The first part of `t` without a JSON form, if any (`stack`: ADTs being visited, so recursive
/// types terminate). Unions can be written (as their active member) and decoded when their
/// members can be told apart.
fn unserializable(cx: &mut Ctx, t: TyId, stack: &mut Vec<TyId>, parse: bool) -> Option<TyId> {
    let fields: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => return None,
        // Already reported.
        TyKind::Error => return None,
        // Literal types are their value (decoding checks it).
        TyKind::Literal(_) => return None,
        // Elements of `never[]` (`JSON.stringify([])`) never exist.
        TyKind::Never if !parse => return None,
        // A dynamic value is written as what it holds, checked at run time (velt_vir
        // `json/any.rs`).
        TyKind::Dyn(..) if !parse && cx.dynamic(t).is_some() => return None,
        TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
        // A `shared<T>` is written as the `T` inside, read in its box (velt_vir `json/write.rs`),
        // when it is read like its `T` (`frozen_readable`): not a `shared<int>`, whose atomics
        // another task may be running, nor one holding a `Mutex`, read only under its lock.
        // Decoding makes no `shared` value.
        TyKind::Shared(e) if !parse && cx.atomic_int(t).is_none() && cx.frozen_readable(e) => {
            vec![e]
        }
        // Tuples are fixed-length arrays.
        TyKind::Tuple(es) => es,
        TyKind::Adt(d, args) => {
            if stack.contains(&t) || cx.is_json_value(d) {
                return None;
            }
            // A function with properties is written as a function is (left out, `null` in an
            // array: velt_vir `json/write.rs`); parsing into one is reported before.
            if !parse && cx.is_callable_object(t) {
                return None;
            }
            // `Record<K, V>` is an object (`record_keys` checked `K` where the type was resolved
            // or instantiated, before this check).
            if Some(d) == cx.prelude_adt("Record") {
                return match args.as_slice() {
                    [_, v] => unserializable(cx, *v, stack, parse),
                    _ => Some(t),
                };
            }
            // `Map<string, V>` is an object; other keys have no JSON form.
            if Some(d) == cx.prelude_adt("Map") {
                return match args.as_slice() {
                    [k, v] if matches!(cx.ty.kind(*k), TyKind::Str) => {
                        unserializable(cx, *v, stack, parse)
                    }
                    _ => Some(t),
                };
            }
            // A class with `toJSON()` is written as what it returns, as in JS (std's `Date` and
            // `URL`, and their subclasses).
            if !parse {
                if let Some((_, ret)) =
                    crate::hooks::final_hook(cx, d, &args, crate::hooks::TO_JSON)
                {
                    stack.push(t);
                    let bad = unserializable(cx, ret, stack, parse);
                    stack.pop();
                    return bad;
                }
            }
            // Decoding would create an instance without running its non-public constructor.
            if parse && cx.ctor_visibility(d) != ast::CtorVisibility::Public {
                return Some(t);
            }
            let tys: Vec<TyId> = match &cx.info[d.0 as usize] {
                // Decoding fills fields without running the constructor: it cannot set what a
                // type keeps to itself. A private field declared by a std type (a runtime
                // handle, also inherited by a user class) has no JSON form at all.
                DefInfo::Adt(a)
                    if a.fields.iter().any(|f| {
                        f.private_to.is_some_and(|o| parse || cx.keeps_handles(o))
                            && !cx.plain_private(f.private_to, &f.name)
                    }) =>
                {
                    return Some(t)
                }
                // Writing skips ES private fields (`#x`), as JavaScript does; `private x` is
                // written, as in Node.
                DefInfo::Adt(a)
                    if matches!(a.kind, AdtKind::Struct | AdtKind::Class | AdtKind::Anon) =>
                {
                    a.fields
                        .iter()
                        .filter(|f| {
                            !cx.plain_private(f.private_to, &f.name)
                                && (parse || !f.name.starts_with(ast::PRIVATE_NAME_PREFIX))
                        })
                        .map(|f| f.ty)
                        .collect()
                }
                // Numeric enums are numbers (their discriminants), string enums their strings.
                DefInfo::Enum(e) if e.variants.iter().all(|v| v.payload.is_empty()) => return None,
                // A union is written as its active member; decoding tells the members apart by
                // the JSON value (see `union_decode_problem`).
                DefInfo::Enum(e) if e.is_union => {
                    let members = e.variants.iter().map(|v| v.payload[0]).collect();
                    if parse && union_decode_problem(cx, t).is_some() {
                        return Some(t);
                    }
                    members
                }
                _ => return Some(t),
            };
            stack.push(t);
            let tys: Vec<TyId> = tys.into_iter().map(|f| cx.subst(f, &args)).collect();
            let bad = tys
                .into_iter()
                .find_map(|f| unserializable(cx, f, stack, parse));
            stack.pop();
            return bad;
        }
        _ => return Some(t),
    };
    fields
        .into_iter()
        .find_map(|f| unserializable(cx, f, stack, parse))
}
