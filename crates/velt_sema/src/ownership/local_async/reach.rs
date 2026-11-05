//! What a callback's locals reach of what it captured, for [`super::changes`] (#854): a local
//! bound to a value computed from a capture may be the captured value itself, or a new value
//! that may only hold it (`new Ctx(app)`, a literal, a library copy, a call of a function that
//! returns new values); a part of a new value is reached when its type can hold the captured
//! value.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{
    AdtKind, Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, PatKind,
    Stmt, StmtKind as S, TyId, TyKind,
};
use crate::ownership::local_closures::walk::{self, Visit};

use super::changes::{callee_name, in_library, Memo, Root, Roots};
use super::fed::field_of;

/// Calls that return a new value, not one reached from their arguments.
const FRESH: &[&str] = &[
    "clone",
    "slice",
    "map",
    "filter",
    "concat",
    "toSorted",
    "toReversed",
    "toSpliced",
    "with",
    "flat",
    "flatMap",
    "structuredClone",
    "split",
    "toString",
    "join",
];

/// The locals of `f` that reach a root (the roots themselves, and what is computed from them),
/// each with its root.
pub(super) fn reached_locals(cx: &mut Ctx, f: &FnDef, roots: &Roots, memo: &mut Memo) -> Roots {
    let mut taint = roots.clone();
    let mut binds = vec![];
    walk::block(&f.body.block, &mut Binds(&mut binds));
    // The bindings each local is read by: a binding is looked at again only when a local it
    // reads is newly reached, so each is evaluated at most once per local it reads (linear in
    // the body, also for a long chain of assignments in a loop).
    let mut readers: FxHashMap<LocalId, Vec<usize>> = FxHashMap::default();
    for (i, (_, from, _)) in binds.iter().enumerate() {
        let (mut read, mut made) = (vec![], vec![]);
        walk::expr(from, &mut Reads(&mut read, &mut made));
        // A closure made or a function called here reads what it captured.
        for d in made {
            if let Some(Def::Fn(k)) = &cx.defs[d.0 as usize] {
                read.extend(k.captures.iter().map(|c| c.outer));
            }
        }
        read.sort_by_key(|l| l.0);
        read.dedup();
        for l in read {
            readers.entry(l).or_default().push(i);
        }
    }
    let mut work: Vec<LocalId> = taint.keys().copied().collect();
    while let Some(l) = work.pop() {
        let Some(is) = readers.get(&l) else { continue };
        for &i in is {
            let (locals, from, parts) = &binds[i];
            // Nothing to learn when every local is reached already.
            if locals.iter().all(|x| taint.get(x).is_some_and(|v| v.1)) {
                continue;
            }
            let Some((r, direct)) = reaches(cx, from, &taint, memo) else {
                continue;
            };
            // A loop variable or a destructured part is what the new value holds.
            let direct = direct || *parts;
            for &x in locals {
                let t = f.body.locals[x.0 as usize].ty;
                let known = taint.get(&x).map(|v| v.1);
                // A local both bound to a new value and to a reached one is reached.
                if known.is_none_or(|d| !d && direct) && !plain(cx, t) {
                    taint.insert(x, (r, direct));
                    work.push(x);
                }
            }
        }
    }
    taint
}

/// The locals an expression reads.
struct Reads<'a>(&'a mut Vec<LocalId>, &'a mut Vec<DefId>);

impl Visit for Reads<'_> {
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Local(l, _) => self.0.push(*l),
            E::Closure(c) => self.1.push(*c),
            E::Call {
                callee: Callee::Def(d, _),
                ..
            } => self.1.push(*d),
            _ => {}
        }
    }
}

/// Locals bound from an expression: `let`, a pattern, a `for…of` binding (from what it walks),
/// a `match` arm, an assignment to the whole local.
/// `parts`: the locals are bound to parts of the value (a loop over it, a destructuring
/// pattern), not to the value itself.
struct Binds<'a>(&'a mut Vec<(Vec<LocalId>, Expr, bool)>);

impl Visit for Binds<'_> {
    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } => self.0.push((vec![*local], init.clone(), false)),
            S::LetPat { pat, init } => self.0.push((bindings(pat), init.clone(), true)),
            S::ForOf { binding, iter, .. } => self.0.push((bindings(binding), iter.clone(), true)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Assign { place, value } => {
                if let E::Local(l, _) = place.kind {
                    self.0.push((vec![l], (**value).clone(), false));
                }
            }
            E::Match { scrutinee, arms } => {
                for a in arms {
                    // `x?.f()` binds the value itself; other patterns bind its parts.
                    let whole = matches!(&a.pat.kind, PatKind::Binding(..))
                        || matches!(&a.pat.kind, PatKind::Some(q) if matches!(q.kind, PatKind::Binding(..)));
                    self.0
                        .push((bindings(&a.pat), (**scrutinee).clone(), !whole));
                }
            }
            _ => {}
        }
    }
}

/// The locals pattern `p` binds.
fn bindings(p: &Pat) -> Vec<LocalId> {
    fn go(p: &Pat, out: &mut Vec<LocalId>) {
        match &p.kind {
            PatKind::Binding(l, _) => out.push(*l),
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|q| go(q, out))
            }
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|q| go(q, out));
                out.extend(rest.iter().copied());
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, q)| go(q, out)),
            PatKind::Some(q) => go(q, out),
            _ => {}
        }
    }
    let mut out = vec![];
    go(p, &mut out);
    out
}

/// The root a value computed by `e` may reach (module docs), and whether it may be the captured
/// value itself (`true`) or is a new value that may only hold it (`false`: `new Ctx(app)`, an
/// object literal, `list.slice()`, a call of a function returning new values). `None` for a value
/// that holds no reference, one behind a `shared`, `Atomic`, `Mutex` or resource, and a closure.
/// A part of a new value (`ctx.app`, `copy[0]`) may be the captured value.
pub(super) fn reaches(
    cx: &mut Ctx,
    e: &Expr,
    taint: &Roots,
    memo: &mut Memo,
) -> Option<(Root, bool)> {
    if plain(cx, e.ty) || sharing(cx, e.ty) {
        return None;
    }
    if let Some(x) = viewed_buffer(cx, e).or_else(|| buffer_of(cx, e)) {
        return reaches(cx, &x, taint, memo);
    }
    match &e.kind {
        E::Local(l, _) => taint.get(l).copied(),
        // A closure handing out a buffer it captured, or a view over one: its result is that buffer.
        E::Closure(c) => held_through(cx, *c, taint, memo),
        E::Lit(_) | E::Global(_) | E::FnRef(..) => None,
        E::Block(b) => b.value.as_deref().and_then(|v| reaches(cx, v, taint, memo)),
        // A conversion keeps the value: a new value converted to an `any` or an interface
        // (`f(ctx)` through `any`) is still the request's own.
        E::Await(x)
        | E::UnwrapSome(x, _)
        | E::Downcast(x)
        | E::Upcast(x)
        | E::WrapSome(x)
        | E::ToDyn { expr: x, .. } => reaches(cx, x, taint, memo),
        // A part of a value that may be the captured one may be captured too; a part of a new
        // value only when its type can hold the captured value (`ctx.app` of `new Ctx(app)`,
        // not `ctx.depth`).
        E::Index { base, .. } => {
            let (r, direct) = reaches(cx, base, taint, memo)?;
            (direct || can_hold(cx, e.ty, r.ty, memo)).then_some((r, true))
        }
        // A field of a new value, also when its type can hold the captured value, is a new value
        // of its own unless some code stores a value that is not new in it (fed.rs): `ctx.out`
        // made by `out: string[] = []` is the request's, `ctx.app` set by `this.app = app` may
        // be the captured value.
        E::Field { base, index, .. } => {
            let (r, direct) = reaches(cx, base, taint, memo)?;
            if direct {
                return Some((r, true));
            }
            if !can_hold(cx, e.ty, r.ty, memo) {
                return None;
            }
            let field = field_of(cx, base.ty, *index);
            let own = match (field, memo.fed.as_ref()) {
                (Some(f), Some(fed)) => !fed.contains(&f),
                _ => false,
            };
            // A field fed by key: the error names where the value was stored.
            if let (false, Some(f)) = (own, field) {
                if let Some(site) = memo.fed_sites.get(&f) {
                    memo.via_site = Some(site.clone());
                }
            }
            Some((r, !own))
        }
        // A function declared in the body, called by name: as a closure handing out what it captured.
        E::Call {
            callee: Callee::Def(d, _),
            ..
        } if held_through(cx, *d, taint, memo).is_some() => held_through(cx, *d, taint, memo),
        _ => {
            let mut found: Option<(Root, bool)> = None;
            walk::children(e, &mut |x| {
                if found.is_none_or(|f| !f.1) {
                    if let Some((r, _)) = reaches(cx, x, taint, memo) {
                        found = Some((r, true));
                    }
                }
            });
            let (r, _) = found?;
            Some((r, !fresh(cx, e, memo)))
        }
    }
}

/// What the closure or function `c` hands out, if it hands out a reached capture (`hands_out_held`).
fn held_through(cx: &mut Ctx, c: DefId, taint: &Roots, memo: &mut Memo) -> Option<(Root, bool)> {
    if !hands_out_held(cx, c, memo) {
        return None;
    }
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return None;
    };
    let found = f.captures.iter().find_map(|k| taint.get(&k.outer).copied());
    found.map(|(r, _)| (r, true))
}

const VIEWS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
    "DataView",
];

fn is_view(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if cx.std_class(*d, VIEWS))
}

fn is_buffer(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if cx.std_class(*d, &["ArrayBuffer"]))
}

/// The `ArrayBuffer` a typed array or `DataView` is made over (`new Uint8Array(b)`): the view writes
/// to it, so the view reaches what the buffer does. Over an array or another typed array it copies.
fn viewed_buffer(cx: &Ctx, e: &Expr) -> Option<Expr> {
    let E::New { def, args, .. } = &e.kind else {
        return None;
    };
    if !cx.std_class(*def, VIEWS) {
        return None;
    }
    args.first().and_then(|a| buffer_in(cx, a))
}

/// `e`, through conversions, if it is an `ArrayBuffer`.
fn buffer_in(cx: &Ctx, e: &Expr) -> Option<Expr> {
    if is_buffer(cx, e.ty) {
        return Some(e.clone());
    }
    match &e.kind {
        E::New { .. }
        | E::Call {
            callee: Callee::Def(..),
            ..
        } => None,
        E::Upcast(x) | E::WrapSome(x) | E::ToDyn { expr: x, .. } => buffer_in(cx, x),
        _ => {
            let mut found = None;
            walk::children(e, &mut |x| {
                found = found.take().or_else(|| buffer_in(cx, x))
            });
            found
        }
    }
}

/// The view whose `.buffer` `e` reads: that buffer is new exactly when the view is.
fn buffer_of(cx: &Ctx, e: &Expr) -> Option<Expr> {
    if !is_buffer(cx, e.ty) {
        return None;
    }
    match &e.kind {
        E::Field { base, .. } if is_view(cx, base.ty) => Some((**base).clone()),
        E::Call { callee, args } if reads_buffer(cx, callee, args) => args.first().cloned(),
        _ => None,
    }
}

/// Is this call a view's `buffer`, `byteLength` or `byteOffset`, which change nothing (the base class's
/// getters, called through its table)?
pub(super) fn reads_buffer(cx: &Ctx, callee: &Callee, args: &[Expr]) -> bool {
    let Some(TyKind::Adt(class, _)) = (args.first()).map(|a| cx.ty.kind(a.ty)) else {
        return false;
    };
    if args.len() != 1 || !cx.std_class(*class, VIEWS) {
        return false;
    }
    let method = match callee {
        Callee::Def(d, _) => Some(*d),
        Callee::Virtual { slot } => cx
            .adt(*class)
            .and_then(|a| a.vtable.get(*slot as usize).copied()),
        _ => None,
    };
    method
        .and_then(|d| callee_name(cx, d))
        .is_some_and(|(n, _)| matches!(n.as_str(), "buffer" | "byteLength" | "byteOffset"))
}

/// Does closure `d`, which returns a buffer or a view, return what it captured or was given, or a
/// view over that? `fresh` would call its result new, as its body may be a `new`.
fn hands_out_held(cx: &mut Ctx, d: DefId, memo: &mut Memo) -> bool {
    let wanted = matches!(&cx.defs[d.0 as usize], Some(Def::Fn(f))
        if !f.captures.is_empty() && (is_view(cx, f.ret) || is_buffer(cx, f.ret)));
    if !wanted {
        return false;
    }
    let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() else {
        return false;
    };
    let roots: Roots = (f.params.iter())
        .map(|p| {
            let ty = f.body.locals[p.local.0 as usize].ty;
            (p.local, (Root { local: p.local, ty }, true))
        })
        .collect();
    let taint = reached_locals(cx, &f, &roots, memo);
    let mut results: Vec<Expr> = f.body.block.value.as_deref().cloned().into_iter().collect();
    struct Returns<'a>(&'a mut Vec<Expr>);
    impl Visit for Returns<'_> {
        fn stmt(&mut self, s: &Stmt) {
            if let S::Return(Some(x)) = &s.kind {
                self.0.push(x.clone());
            }
        }
    }
    walk::block(&f.body.block, &mut Returns(&mut results));
    let held = results
        .iter()
        .any(|r| reaches(cx, r, &taint, memo).is_some_and(|(_, direct)| direct));
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    held
}

/// Is `e` a new value: `new`, a literal, a builtin copy, a call of a function that returns new
/// values (`crate::fresh_returns`), or a library call that returns a copy ([`FRESH`])?
pub(super) fn fresh(cx: &mut Ctx, e: &Expr, memo: &mut Memo) -> bool {
    // `new` always makes a new object (whatever its constructor keeps), as a literal and a
    // spread object (`{ ...cfg }` in a generic function) do.
    if matches!(
        e.kind,
        E::New { .. }
            | E::ArrayLit(_)
            | E::AdtLit { .. }
            | E::Call {
                callee: Callee::Intrinsic(Intrinsic::SpreadObject),
                ..
            }
    ) {
        return true;
    }
    if let E::Call {
        callee: Callee::Def(d, _),
        ..
    } = &e.kind
    {
        if in_library(cx, *d)
            && callee_name(cx, *d).is_some_and(|(n, _)| FRESH.contains(&n.as_str()))
        {
            return true;
        }
    }
    let Some(ds) = crate::fresh_returns::fresh_callees(e) else {
        return false;
    };
    ds.into_iter().all(|d| {
        if let Some(&b) = memo.fresh.get(&d) {
            return b;
        }
        let set = crate::fresh_returns::fresh_functions(cx, std::iter::once(d));
        for &x in &set {
            memo.fresh.insert(x, true);
        }
        let b = set.contains(&d);
        memo.fresh.insert(d, b);
        b
    })
}

/// The constructor `new` of class `d` runs: its own, or the nearest base class's.
pub(super) fn constructor(cx: &Ctx, d: DefId) -> Option<DefId> {
    let mut d = d;
    for _ in 0..64 {
        let a = cx.adt(d)?;
        if let Some(c) = a.ctor {
            return Some(c);
        }
        d = match cx.ty.kind(a.base?) {
            TyKind::Adt(b, _) => *b,
            _ => return None,
        };
    }
    None
}

/// Can a value of type `part` (a part of a new value) be, or hold, a value of type `root` or
/// something it holds? A type parameter or an `any` may hold anything.
pub(super) fn can_hold(cx: &mut Ctx, part: TyId, root: TyId, memo: &mut Memo) -> bool {
    let from_part = held_types(cx, part, memo);
    if from_part.iter().any(|&t| open(cx, t)) {
        return true;
    }
    let from_root = held_types(cx, root, memo);
    if from_root.iter().any(|&t| open(cx, t)) {
        return true;
    }
    let mut both = from_part.intersection(&from_root);
    both.any(|&t| !plain(cx, t))
}

/// May a value of `t` be of any type (a type parameter, `any`, `unknown`, `object`)?
fn open(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Param(_)) || cx.dynamic(t).is_some()
}

/// The types a value of type `t` may be or hold, through fields (also of subclasses), elements,
/// entries, union members and interface implementations (memoized).
fn held_types(cx: &mut Ctx, t: TyId, memo: &mut Memo) -> FxHashSet<TyId> {
    if let Some(s) = memo.holds.get(&t) {
        return s.clone();
    }
    let mut seen: FxHashSet<TyId> = FxHashSet::default();
    let mut work = vec![t];
    while let Some(x) = work.pop() {
        if !seen.insert(x) {
            continue;
        }
        match cx.ty.kind(x).clone() {
            TyKind::Adt(d, args) => work.extend(super::types::adt_parts(cx, d, &args)),
            TyKind::Dyn(iface, _) => {
                let impls: Vec<TyId> = (cx.impls.iter())
                    .filter(|i| i.iface == iface)
                    .map(|i| i.ty)
                    .collect();
                work.extend(impls);
            }
            k => work.extend(crate::types::children(&k)),
        }
    }
    memo.holds.insert(t, seen.clone());
    seen
}

/// Does a value of `t` hold no reference (a number, a boolean, a string, …)? A type parameter
/// may stand for an object.
pub(super) fn plain(cx: &mut Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => false,
        TyKind::Str => true,
        _ => cx.is_copy(t),
    }
}

/// Is a value of `t` changed in place for every thread (a `shared` value, an `Atomic`, a
/// `Mutex`, a library's handle struct such as a `Channel` or a `TcpStream`, whose copies share
/// what they handle), or shared with each call (a resource without `clone()`)?
pub(super) fn sharing(cx: &mut Ctx, t: TyId) -> bool {
    let handle = match cx.ty.kind(t) {
        TyKind::Adt(d, _) | TyKind::Dyn(d, _) => cx
            .adt(*d)
            .is_some_and(|a| a.kind == AdtKind::Struct && cx.scopes[a.module].is_library),
        TyKind::Option(inner, _) => {
            let inner = *inner;
            return sharing(cx, inner);
        }
        _ => false,
    };
    handle
        || matches!(cx.ty.kind(t), TyKind::Shared(_))
        || cx.atomic_value(t).is_some()
        || cx.mutex_value(t).is_some()
        || cx.owns_uncopyable(t)
}
