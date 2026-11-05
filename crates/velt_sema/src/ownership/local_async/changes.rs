//! Changes a callback that a library runs on other threads makes to what it captured (#854). A
//! server's handler runs as a copy on each connection's task, each call with its own copy of
//! what the handler captured ([`super`]), so a change it makes to a captured object is made to
//! that copy and lost: Node would see `1 2 3 4 5` where Velt answered `1 1 1 1 1`. [`super`]
//! reports a write to a captured variable's own place (`c.count++`); this pass, which runs once
//! mutation inference has decided which calls change their arguments (`super::super::infer`),
//! reports the rest, erring toward an error over a silent copy:
//!
//! - **What is reached from a capture**: a local bound to a value computed from it (`const u =
//!   users.get(id)!`, `const a = t`, an `any` holding it, a `for…of` or destructuring binding, an
//!   optional chain or a conditional over it), and the result of a call given it (`m.get(k)!`,
//!   `xs.find(…)`), but not values that cannot hold a reference (numbers, strings, booleans).
//! - **New values** given a captured one only *hold* it: `new Ctx(app)`, an object or array
//!   literal, a spread object, a library copy (`slice`, `map`, `clone`, …) and a call of a
//!   function that returns new values (`crate::fresh_returns`). Their own fields are the
//!   request's (`ctx.depth++`); a part of one may be the captured value when its type can hold
//!   it (`ctx.app`, `copy[0]`), and is then reached.
//! - **A change**: a write into a place whose base is reached (`users.get("a")!.visits++`), a
//!   call that takes a reached value mutably (a library's method or an intrinsic, by the mode
//!   mutation inference gave the argument, as for `m.set(k, v)` or `xs.push(x)`; an overridden or
//!   interface method, by its joined mode), and a call of a function of the program whose body
//!   changes that parameter (followed the same way, an `async` one too), and a closure the
//!   callback makes, whose captures and, when it is passed to a call with a reached argument
//!   (`users.forEach((u) => …)`), whose parameters stand for what the callback reached, and a
//!   constructor of the program (also an inherited one) that changes what it is given.
//!
//! The error names the field written, and suggests sharing the object. Not reported: what a
//! `shared` value, an `Atomic` or a `Mutex` holds (changed in place for every thread); a captured
//! resource without `clone()` (the call shares it with the callback); a few library methods that
//! take their receiver mutably only to fill a cache while reading or to keep a cursor that each
//! copy may keep for itself ([`READERS`]: `Headers.get`, `RegExp.test`, …); a call through a
//! function value (its callee is not known here: a callback stored in a captured object that
//! assigns what *it* captured is reported by `super::callbacks`); and a parameter the callee
//! assigns before changing it (`o = o.clone(); o.n++`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, PassMode, TyId, TyKind,
    UseMode,
};
use crate::ownership::local_closures::walk::{self, Visit};

use super::reach::{constructor, plain, reached_locals, reaches};
use super::targets::Read;

/// A callback that a library runs on other threads: the closure, and where (with the label) the
/// program hands it to the library.
pub(crate) struct Crossing {
    pub closure: DefId,
    pub at: Span,
    pub label: String,
}

/// Bodies deeper than this are not followed (a change found deeper is not reported).
const MAX_DEPTH: u32 = 48;

/// Library methods that take their receiver mutably without changing what a program reads of
/// it: a cache filled while reading (`Headers`, `URLSearchParams`), or a cursor each copy may
/// keep for itself (`RegExp.lastIndex`, a `TextDecoder`'s pending bytes). `(class, methods)`.
const READERS: &[(&str, &[&str])] = &[
    (
        "Headers",
        &[
            "get",
            "has",
            "getSetCookie",
            "forEach",
            "entries",
            "keys",
            "values",
            "toString",
        ],
    ),
    (
        "URLSearchParams",
        &[
            "get", "getAll", "has", "forEach", "entries", "keys", "values", "toString", "size",
        ],
    ),
    ("URL", &["toString", "toJSON"]),
    ("RegExp", &["test", "exec", "toString"]),
    ("TextDecoder", &["decode"]),
];

/// Report the changes the recorded crossing callbacks make to what they captured (module
/// docs). Runs after `infer_modes`.
pub(crate) fn check_crossing_changes(cx: &mut Ctx) {
    let list = std::mem::take(&mut cx.crossing_callbacks);
    if list.is_empty() {
        return;
    }
    // A callback that captures nothing of the program's (`async () => new Response(…)`) has
    // nothing to change: the scans below are skipped when no callback captures anything.
    let captures = list.iter().any(|x| match &cx.defs[x.closure.0 as usize] {
        Some(Def::Fn(f)) => {
            (f.captures.iter()).any(|c| !f.body.locals[c.inner.0 as usize].name.starts_with('#'))
        }
        _ => false,
    });
    if !captures {
        return;
    }
    // Before any body is taken out of `cx.defs` below, so that every store is seen.
    let lits = std::rc::Rc::new(super::targets::Literals::scan(cx));
    let roots: Vec<DefId> = list.iter().map(|x| x.closure).collect();
    let keyed = super::keyed::keyed_fields(cx, &lits, &roots);
    let fed = std::rc::Rc::new(super::fed::fed_fields(cx, keyed.keys().copied().collect()));
    let fed_sites = std::rc::Rc::new(keyed);
    let mut reported: FxHashSet<(DefId, String)> = FxHashSet::default();
    for x in list {
        let Some(Def::Fn(f)) = cx.defs[x.closure.0 as usize].take() else {
            continue;
        };
        // One per callback: an answer found while another body was being walked (a recursive
        // call) is not reused for the next callback.
        let mut memo = Memo {
            fed: fed.clone(),
            fed_sites: fed_sites.clone(),
            lits: lits.clone(),
            ..Memo::default()
        };
        let span = f.span;
        // What [`super`] reported already (a write in the handler itself) is not reported again.
        let roots: Roots = (f.captures.iter())
            .filter(|c| {
                let name = &f.body.locals[c.inner.0 as usize].name;
                // `#…`: a value the compiler captured, not the program's.
                !name.starts_with('#')
                    && !cx
                        .reported_captures
                        .iter()
                        .any(|(n, s)| n == name && *s == span)
            })
            .map(|c| {
                let ty = f.body.locals[c.inner.0 as usize].ty;
                (c.inner, (Root { local: c.inner, ty }, true))
            })
            .collect();
        let mut found = vec![];
        changes(
            cx,
            &f,
            &roots,
            &FxHashMap::default(),
            0,
            &mut memo,
            &mut found,
        );
        let what = if f.is_async {
            "async closure"
        } else {
            "closure"
        };
        for (cap, at, place, via, site) in found {
            let local = &f.body.locals[cap.0 as usize];
            let (name, ty) = (local.name.clone(), local.ty);
            if !reported.insert((x.closure, name.clone())) {
                continue;
            }
            report(
                cx,
                what,
                &name,
                ty,
                at,
                (&place, via.as_deref(), site.as_ref()),
                &x,
            );
        }
        cx.defs[x.closure.0 as usize] = Some(Def::Fn(f));
    }
}

#[allow(clippy::too_many_arguments)]
fn report(
    cx: &mut Ctx,
    what: &str,
    name: &str,
    ty: TyId,
    at: Span,
    (place, via, site): (&str, Option<&str>, Option<&super::keyed::Site>),
    x: &Crossing,
) {
    // `place` starts with the capture when it is written through it directly (`c.count`), and
    // with `…` when it is reached through a value computed from it (`….visits`).
    let direct = place == name || place.starts_with(&format!("{name}."));
    let last = place
        .rsplit_once('.')
        .map(|(_, f)| f)
        .filter(|f| !f.is_empty());
    let field = match (last, direct) {
        (Some(f), true) => format!("`{f}` of captured `{name}`"),
        (Some(f), false) => format!("`{f}` of a value reached from captured `{name}`"),
        (None, true) => format!("captured `{name}`"),
        (None, false) => format!("a value reached from captured `{name}`"),
    };
    let through = match via {
        Some(v) if direct => format!(" (`{v}` changes `{place}`)"),
        Some(v) => format!(" (through `{v}`)"),
        None => String::new(),
    };
    let fix = share_fix(cx, name, ty);
    // A field of a new value fed by key: name the value and where it was stored, not a field
    // the captured value does not have (#2081).
    let msg = match site {
        Some(s) => format!(
            "this {what} changes `{place}`, which may be a value reached from captured `{name}`: {} stores one there by key, so it must stay on the task that created it",
            s.text
        ),
        None => format!(
            "this {what} changes {field}{through}, so it must stay on the task that created it"
        ),
    };
    let mut d = Diagnostic::error(msg, at).with_label(x.at, x.label.clone());
    if let Some(s) = site {
        d = d.with_label(s.span, format!("stored here: {}", s.text));
    }
    let d = d
    .with_note(format!(
        "each call runs on another thread with its own copy of `{name}`, so the change would be lost: {fix}"
    ));
    cx.error(d);
}

/// Locals reaching a root: the root, and whether the local may hold the captured value itself
/// (`false`: a new value that may only hold it, see [`reaches`]).
pub(super) type Roots = FxHashMap<LocalId, (Root, bool)>;

/// A root: the local standing for a captured value in the body it belongs to, and its type
/// (what a new value's parts must be able to hold to hold it).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Root {
    pub local: LocalId,
    pub ty: TyId,
}

/// What the walks of one callback remember: whether a function changes its parameter (given
/// whether the argument may be the captured value itself), and whether one returns new values.
#[derive(Default)]
pub(super) struct Memo {
    pub(super) calls: FxHashMap<(DefId, usize, bool, TyId), Option<String>>,
    pub fresh: FxHashMap<DefId, bool>,
    /// The types a value of a type may hold (itself, its fields, elements, …).
    pub holds: FxHashMap<TyId, FxHashSet<TyId>>,
    /// The fields some code stores a value that is not new in (fed.rs), found once per check;
    /// `None`: any field may be.
    pub fed: std::rc::Rc<Option<FxHashSet<super::fed::Field>>>,
    /// The functions object literals store in their fields (targets.rs), found once per check.
    pub lits: std::rc::Rc<super::targets::Literals>,
    /// Where each field fed by key was first fed (keyed.rs), for the error.
    pub fed_sites: std::rc::Rc<FxHashMap<super::fed::Field, super::keyed::Site>>,
    /// The site of the field fed by key that the last [`super::reach::reaches`] went through.
    pub via_site: Option<super::keyed::Site>,
}

/// One change found in a body: (root, where, the place written, the call it goes through).
/// The last part: where a field fed by key on the way got the captured value (keyed.rs).
type Found = (
    LocalId,
    Span,
    String,
    Option<String>,
    Option<super::keyed::Site>,
);

/// The changes body `f` makes to what the locals of `roots` (each standing for a root of the
/// callback being checked) reach, first one per root. `params`: closures whose parameters stand
/// for a root (a closure passed to a call with a reached argument).
pub(super) fn changes(
    cx: &mut Ctx,
    f: &FnDef,
    roots: &Roots,
    params: &FxHashMap<DefId, Root>,
    depth: u32,
    memo: &mut Memo,
    out: &mut Vec<Found>,
) {
    if roots.is_empty() {
        return;
    }
    let taint = reached_locals(cx, f, roots, memo);
    let mut events = vec![];
    walk::block(&f.body.block, &mut Events(&mut events));
    let names: Vec<String> = f.body.locals.iter().map(|l| l.name.clone()).collect();
    // Closures passed to a call with a reached argument: their parameters are reached.
    let mut passed: FxHashMap<DefId, Root> = params.clone();
    for ev in events {
        match ev {
            Event::Write(place, span) => {
                let base = match &place.kind {
                    E::Field { base, .. }
                    | E::Index { base, .. }
                    | E::UnwrapSome(base, _)
                    | E::Downcast(base) => base,
                    _ => continue,
                };
                // Only a write into a captured object (or one reached from it) is lost; a new
                // object's own fields are the request's (`ctx.depth++` on `new Ctx(app)`).
                memo.via_site = None;
                let Some((r, true)) = reaches(cx, base, &taint, memo) else {
                    continue;
                };
                let site = memo.via_site.take();
                if !out.iter().any(|x| x.0 == r.local) {
                    out.push((r.local, span, text(cx, &names, &place), None, site));
                }
            }
            Event::Call(callee, args, span) => {
                let mut reached: Vec<(usize, Root, bool, Option<super::keyed::Site>)> = vec![];
                for (i, a) in args.iter().enumerate() {
                    memo.via_site = None;
                    if let Some((r, direct)) = reaches(cx, a, &taint, memo) {
                        reached.push((i, r, direct, memo.via_site.take()));
                    }
                }
                // A closure given to a call with a reached argument may be handed what that
                // argument holds (`users.forEach((u) => …)`).
                if let Some(&(_, r, _, _)) = reached.first() {
                    for a in &args {
                        if let E::Closure(k) = a.kind {
                            passed.entry(k).or_insert(r);
                        }
                    }
                }
                for (i, r, direct, site) in reached {
                    let a = &args[i];
                    // A loop's own iterator advancing (`for (const x of m)`): its position is
                    // the loop's, not what it walks.
                    let cursor = matches!(a.kind, E::Local(l, _) if names[l.0 as usize].starts_with("<iterator"));
                    if out.iter().any(|x| x.0 == r.local) || cursor {
                        continue;
                    }
                    let base = text(cx, &names, a);
                    let Some((place, via)) =
                        call_changes(cx, (&callee, &args, i), (direct, r.ty), &base, depth, memo)
                    else {
                        continue;
                    };
                    out.push((r.local, span, place, Some(via), site));
                }
            }
            Event::New(class, args, span) => {
                // A constructor of the program that changes what it is given (`t.n++` in
                // `constructor(t: T)`), followed like a function.
                let Some(ctor) = constructor(cx, class) else {
                    continue;
                };
                if in_library(cx, ctor) {
                    continue;
                }
                for (i, a) in args.iter().enumerate() {
                    memo.via_site = None;
                    let Some((r, direct)) = reaches(cx, a, &taint, memo) else {
                        continue;
                    };
                    let site = memo.via_site.take();
                    if out.iter().any(|x| x.0 == r.local) {
                        continue;
                    }
                    let Some(written) =
                        callee_changes(cx, ctor, i + 1, (direct, r.ty), depth + 1, memo)
                    else {
                        continue;
                    };
                    let base = text(cx, &names, a);
                    let place = match written.split_once('.') {
                        Some((_, rest)) => format!("{base}.{rest}"),
                        None => base.clone(),
                    };
                    let name = cx.adt(class).map(|a| a.name.clone()).unwrap_or_default();
                    out.push((r.local, span, place, Some(format!("new {name}(…)")), site));
                }
            }
            Event::Closure(k) => {
                if depth > MAX_DEPTH {
                    continue;
                }
                let Some(Def::Fn(kf)) = cx.defs[k.0 as usize].take() else {
                    continue;
                };
                let mut kroots: Roots = (kf.captures.iter())
                    .filter_map(|c| taint.get(&c.outer).map(|&r| (c.inner, r)))
                    .collect();
                if let Some(&r) = passed.get(&k) {
                    for p in kf.params.iter().skip(kf.captures.len()) {
                        kroots.entry(p.local).or_insert((r, true));
                    }
                }
                let mut inner = vec![];
                changes(cx, &kf, &kroots, &passed, depth + 1, memo, &mut inner);
                cx.defs[k.0 as usize] = Some(Def::Fn(kf));
                for x in inner {
                    if !out.iter().any(|y| y.0 == x.0) {
                        out.push(x);
                    }
                }
            }
        }
    }
}

/// Does the call of `callee` change its argument `i` (`a`, written `base`, reached from a
/// root of type `root_ty`; `direct`: it may be the captured value itself)? The place it changes
/// as the caller sees it, and the call as written.
#[allow(clippy::too_many_arguments)]
fn call_changes(
    cx: &mut Ctx,
    (callee, args, i): (&Callee, &[Expr], usize),
    (direct, root_ty): (bool, TyId),
    base: &str,
    depth: u32,
    memo: &mut Memo,
) -> Option<(String, String)> {
    let a = &args[i];
    let mutable = top_mode(a) == Some(UseMode::BorrowMut);
    match callee {
        Callee::Def(d, _) => {
            let d = *d;
            let (name, method) = callee_name(cx, d).unwrap_or_default();
            let via = match method && i == 0 {
                true => format!("{base}.{name}(…)"),
                false => format!("{name}(…)"),
            };
            if in_library(cx, d) {
                // A new value (`slice()`'s array, `new Map(…)`) changed by the library is the
                // request's: only what it holds may be captured.
                if !direct {
                    return None;
                }
                // A library's body is not looked into: the mode the parameter (or the
                // argument) got says whether the call changes it, unless it only reads.
                let param = match &cx.defs[d.0 as usize] {
                    Some(Def::Fn(f)) => f.params.get(f.captures.len() + i).map(|p| p.mode),
                    _ => None,
                };
                let changes = mutable || param == Some(PassMode::BorrowMut);
                if !changes || reader(cx, d) {
                    return None;
                }
                return Some((base.to_string(), via));
            }
            // A method of an object type runs the function its literals store in the field
            // (a call through an interface it alone implements calls it directly).
            let lits = memo.lits.clone();
            if let Some(targets) = super::targets::forwarded(cx, &lits, d) {
                let Some(targets) = targets.filter(|t| !t.iter().any(|&(g, _)| in_library(cx, g)))
                else {
                    // Functions not all known here may change it, whatever the mode.
                    return Some((base.to_string(), via));
                };
                return follow(cx, &targets, (i, direct, root_ty), (base, via), depth, memo);
            }
            let written = callee_changes(cx, d, i, (direct, root_ty), depth + 1, memo)?;
            // The place the callee writes, as the caller sees it (`this.count` of
            // `c.handle()` is `c.count`).
            let place = match written.split_once('.') {
                Some((_, rest)) => format!("{base}.{rest}"),
                None => base.to_string(),
            };
            Some((place, via))
        }
        // A write or a method call through `any` (`a.n = 5` on a captured `any`): the
        // receiver may change.
        Callee::Intrinsic(
            Intrinsic::AnySet | Intrinsic::AnySetIndex | Intrinsic::AnyDelete | Intrinsic::AnyCall,
        ) if i == 0 && direct => Some((base.to_string(), format!("{base}.… = …"))),
        // A call through a function value (`hooks[0](ctx)`, `plugin.server?.setup?.(ctx)`), or
        // of a function read from an `any` (`f(ctx)` of `f = plugin[hook]`): any of the
        // program's function values that accept the arguments may run, each followed; one made
        // outside the program (a host's, a native package's) may change anything it is given,
        // the captured value or a new value holding it (write-effects.md "Function values").
        Callee::Indirect(fv) => {
            let via = format!("…({base})");
            let (want, read): (Vec<TyId>, Read) = match cx.ty.kind(fv.ty) {
                TyKind::FnPtr { params, .. } => (params.clone(), Read::Typed),
                _ => (args.iter().map(|a| a.ty).collect(), Read::Untyped),
            };
            let lits = memo.lits.clone();
            match super::targets::value_targets(cx, &lits, &want, read, 0) {
                None => Some((base.to_string(), via)),
                Some(t) => follow(cx, &t, (i, direct, root_ty), (base, via), depth, memo),
            }
        }
        Callee::Intrinsic(Intrinsic::AnyApply) if i >= 2 => {
            let via = format!("…({base})");
            let want: Vec<TyId> = args[2..].iter().map(|a| unconverted(a).ty).collect();
            let lits = memo.lits.clone();
            match super::targets::value_targets(cx, &lits, &want, Read::FromAny, 2) {
                None => Some((base.to_string(), via)),
                Some(t) => follow(cx, &t, (i, direct, root_ty), (base, via), depth, memo),
            }
        }
        // `(xs as string[]).push(x)`, `xs!.push(x)`, `u.items.push(x)` of a union `u`: a receiver
        // computed from the value is no place, so it has no mode, and the array still changes.
        // The other intrinsics that change an array are std's own, whose bodies are not followed.
        Callee::Intrinsic(Intrinsic::ArrayPush | Intrinsic::ArrayPop)
            if i == 0 && direct && top_mode(a).is_none() =>
        {
            Some((base.to_string(), format!("{base}.…(…)")))
        }
        // An operation of the compiler's on a new value (`out.push(x)`) changes only it.
        Callee::Intrinsic(_) if mutable && direct => {
            Some((base.to_string(), format!("{base}.…(…)")))
        }
        // An overridden or interface method, or a method of a type parameter's bound: the mode
        // joined over what may be called. A new value (`plugin.render(ctx)` of `new Ctx(app)`)
        // is changed only where one of the methods the call may run changes what it may hold,
        // followed like a function whatever the mode (`c.app.n++` borrows `c` only to read
        // `app`); the library's are not looked into, and a call whose methods are not all known
        // here changes it.
        Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. }
            if mutable || !direct || top_mode(a).is_none() =>
        {
            let via = format!("{base}.…(…)");
            let lits = memo.lits.clone();
            // A receiver computed from the value has no mode to go by: followed too.
            let computed = direct && !mutable;
            let targets = (!direct || computed)
                .then(|| super::targets::method_targets(cx, &lits, callee, args))
                .flatten()
                .filter(|t| !t.iter().any(|&(g, _)| in_library(cx, g)));
            // Methods not all known here may change what a new value holds whatever the mode
            // (`c.app.n++` borrows `c` only to read `app`); the captured value itself, when the
            // mode says so.
            let Some(targets) = targets else {
                return Some((base.to_string(), via));
            };
            follow(
                cx,
                &targets,
                (i, computed, root_ty),
                (base, via),
                depth,
                memo,
            )
        }
        _ => None,
    }
}

/// The value `a` was before it was converted to an `any` for a call through one.
pub(super) fn unconverted(a: &Expr) -> &Expr {
    match &a.kind {
        E::ToDyn { expr, .. } => unconverted(expr),
        _ => a,
    }
}

/// Does one of the methods a call may run (`targets`, from targets.rs) change its argument `i`
/// (written `base`; `direct`: it may be the captured value itself, else a new value that may
/// hold it)? The place as the caller sees it, and the call as written (`via`).
fn follow(
    cx: &mut Ctx,
    targets: &[super::targets::Target],
    (i, direct, root_ty): (usize, bool, TyId),
    (base, via): (&str, String),
    depth: u32,
    memo: &mut Memo,
) -> Option<(String, String)> {
    for &(g, skip) in targets {
        // A literal's function takes no `this`: the receiver is not one of its parameters.
        let Some(k) = i.checked_sub(skip) else {
            continue;
        };
        // A package's function (a value from its code in a route table) is judged as a direct
        // call of it is: by whether it changes that parameter. A function used as a value never
        // has a `BorrowMut` parameter, so the parameter's own flag says it.
        if in_library(cx, g) {
            let Some(Def::Fn(f)) = &cx.defs[g.0 as usize] else {
                continue;
            };
            let changed = (f.params.get(f.captures.len() + k)).is_some_and(|p| {
                p.mode == PassMode::BorrowMut || f.body.locals[p.local.0 as usize].mutable
            });
            if changed && direct {
                return Some((base.to_string(), via));
            }
            continue;
        }
        if let Some(written) = callee_changes(cx, g, k, (direct, root_ty), depth + 1, memo) {
            let place = match written.split_once('.') {
                Some((_, rest)) => format!("{base}.{rest}"),
                None => base.to_string(),
            };
            return Some((place, via));
        }
    }
    None
}

/// Is function `d` one of the library's [`READERS`], or a step of a loop over a collection?
fn reader(cx: &Ctx, d: DefId) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
        return false;
    };
    // A method named by a symbol of the compiler's (`[mapAdvance]`, how a `for…of` loop steps
    // through a collection) only reads it.
    if f.name.contains('\u{FFFF}') {
        return true;
    }
    let name = f.name.rsplit("::").next().unwrap_or(&f.name);
    let Some((class, method)) = name.rsplit_once('.') else {
        return false;
    };
    let class = class.split('<').next().unwrap_or(class);
    READERS
        .iter()
        .any(|(c, ms)| *c == class && ms.contains(&method))
}

/// Does the function `d` change what its parameter `i` (`this` first) reaches, given an argument
/// reached from a root of type `root_ty` (`direct`: the argument may be the captured value
/// itself)? The place it writes, as written there (`this.count`).
pub(super) fn callee_changes(
    cx: &mut Ctx,
    d: DefId,
    i: usize,
    (direct, root_ty): (bool, TyId),
    depth: u32,
    memo: &mut Memo,
) -> Option<String> {
    let key = (d, i, direct, root_ty);
    if let Some(r) = memo.calls.get(&key) {
        return r.clone();
    }
    if depth > MAX_DEPTH {
        return None;
    }
    // A body being walked further up (a recursive call) finds no change here; the walk up
    // there finds it.
    let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() else {
        return None;
    };
    memo.calls.insert(key, None);
    let mut found = vec![];
    // A closure's leading params are its captures.
    if let Some(p) = f.params.get(f.captures.len() + i) {
        let ty = f.body.locals[p.local.0 as usize].ty;
        // A parameter the body assigns may hold another object when it is changed
        // (`o = o.clone(); o.n++`): not followed.
        if !plain(cx, ty) && !reassigns(&f, p.local) {
            // The parameter stands for the caller's root: what a new value holds is judged by
            // the captured value's type, not the parameter's.
            let root = Root {
                local: p.local,
                ty: root_ty,
            };
            let roots = FxHashMap::from_iter([(p.local, (root, direct))]);
            changes(
                cx,
                &f,
                &roots,
                &FxHashMap::default(),
                depth,
                memo,
                &mut found,
            );
        }
    }
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    let r = found.into_iter().next().map(|x| x.2);
    memo.calls.insert(key, r.clone());
    r
}

/// Does body `f` assign local `l` as a whole?
fn reassigns(f: &FnDef, l: LocalId) -> bool {
    struct Assigns(LocalId, bool);
    impl Visit for Assigns {
        fn expr(&mut self, e: &Expr) {
            if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                if matches!(place.kind, E::Local(x, _) if x == self.0) {
                    self.1 = true;
                }
            }
        }
    }
    let mut v = Assigns(l, false);
    walk::block(&f.body.block, &mut v);
    v.1
}

pub(super) fn in_library(cx: &Ctx, d: DefId) -> bool {
    cx.try_fn(d).is_some_and(|i| cx.scopes[i.module].is_library)
}

/// The fix for a change to `name` of type `ty` that every call would make to its own copy.
fn share_fix(cx: &Ctx, name: &str, ty: TyId) -> String {
    let rest = "or keep what changes in a `shared(...)` or an `Atomic`";
    if name == "this" {
        return format!(
            "share the object the method runs on (create it as `shared(new Mutex(…))` and change it with `.with(…)`), pass what the handler needs as values, {rest}"
        );
    }
    match cx.ty.kind(ty) {
        TyKind::Adt(..) => format!(
            "share the object, `const {name} = shared(new Mutex(new {}(…)))` changed with `{name}.with(…)`, {rest}",
            cx.display(ty)
        ),
        _ => format!(
            "share the value, `const {name} = shared(new Mutex(…))` changed with `{name}.with(…)`, {rest}"
        ),
    }
}

/// What happens in a body, in the order it is written.
enum Event {
    /// A place assigned (not a whole local).
    Write(Expr, Span),
    /// A call, with its arguments (`this` first for a method).
    Call(Callee, Vec<Expr>, Span),
    /// `new C(args)`: its constructor runs with `args` after its `this`.
    New(DefId, Vec<Expr>, Span),
    /// A closure made here.
    Closure(DefId),
}

struct Events<'a>(&'a mut Vec<Event>);

impl Visit for Events<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                if !matches!(place.kind, E::Local(..)) {
                    self.0.push(Event::Write((**place).clone(), e.span));
                }
            }
            E::Call { callee, args } => {
                self.0
                    .push(Event::Call(callee.clone(), args.clone(), e.span));
            }
            E::Closure(k) => self.0.push(Event::Closure(*k)),
            E::New { def, args, .. } => self.0.push(Event::New(*def, args.clone(), e.span)),
            _ => {}
        }
    }
}

/// The use mode of place `e`'s outermost node.
fn top_mode(e: &Expr) -> Option<UseMode> {
    match &e.kind {
        E::Local(_, m) | E::Field { mode: m, .. } | E::Index { mode: m, .. } => Some(*m),
        E::UnwrapSome(_, m) => Some(*m),
        _ => None,
    }
}

/// The name of function `d` (`handle` of `C.handle`), and whether it is a method.
pub(super) fn callee_name(cx: &Ctx, d: DefId) -> Option<(String, bool)> {
    let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
        return None;
    };
    let name = f.name.rsplit("::").next().unwrap_or(&f.name);
    let short = name.rsplit('.').next().unwrap_or(name).to_string();
    let method = cx.try_fn(d).is_some_and(|i| i.this.is_some());
    Some((short, method))
}

/// Place `e` as written (`c.count`; `…` for a part computed by a call), for the message.
fn text(cx: &Ctx, names: &[String], e: &Expr) -> String {
    match &e.kind {
        // A local of the compiler's (`<chain@…>` of an optional chain) is not written.
        E::Local(l, _) => match names.get(l.0 as usize) {
            Some(n) if !n.starts_with('<') => n.clone(),
            _ => "…".to_string(),
        },
        E::Field { base, index, .. } => {
            format!(
                "{}.{}",
                text(cx, names, base),
                field_name(cx, base.ty, *index)
            )
        }
        E::Index { base, .. } => format!("{}[…]", text(cx, names, base)),
        E::UnwrapSome(base, _) | E::Downcast(base) | E::UnwrapVariant { expr: base, .. } => {
            text(cx, names, base)
        }
        _ => "…".to_string(),
    }
}

fn field_name(cx: &Ctx, t: TyId, index: u32) -> String {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => match cx.info.get(d.0 as usize) {
            Some(crate::defs::DefInfo::Adt(a)) => (a.fields.get(index as usize))
                .map(|f| f.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}
