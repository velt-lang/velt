//! Exclusive access (docs/reference/memory.md "Exclusive access"): within one call, a place the callee
//! may modify is reachable through no other argument. Checked over the final HIR (after
//! ownership / mutation inference and the move analysis, so pass modes and clones are settled).
//!
//! Per call (`Call`, `New`), each argument's [`uses`] are compared pairwise:
//! - a place passed `BorrowMut` (an argument or receiver the callee is inferred to modify, a
//!   variable a closure argument modifies, the receiver of `Mutex.with` unless it is a
//!   `shared` handle) must not overlap any place another argument borrows, mutably borrows,
//!   moves or captures;
//! - a place passed `Borrow` must not overlap a place another argument moves;
//! - no argument may mutate or move (while being evaluated) a place another argument passes by
//!   reference, except change the inside of a class instance, object or array passed so (the
//!   callee sees the change as JavaScript's would: `this.set(this.src.get(1))`).
//!
//! Places overlap when one is a prefix of the other (`a` / `a.f`, `xs` / `xs[i]`; `xs[i]` and
//! `xs[j]` always overlap, `a.f` and `a.g` never do); borrowing pattern bindings count as
//! the place they borrow from. The non-Copy params of closures and of named functions used as
//! values may alias each other (calls through a function value don't check their arguments
//! against each other), so there they count as one place, unless their types cannot be one
//! value (instances of unrelated classes, an array and a class instance). Guarantees for code generation: during a call, memory reachable
//! from a `BorrowMut` parameter is not reachable from any other parameter (including the
//! captures of a closure argument), and `Borrow` parameters may alias each other but never a
//! `BorrowMut` or `Owned` one.

mod iteration;
mod let_borrow;
mod this_alias;
mod uses;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::defs::FnKind;
use crate::hir::{
    Callee, Capture, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalDef, LocalId,
    PassMode, Pat, PatKind, Stmt, StmtKind as S, TyKind, UseMode,
};
use crate::ownership::validate::place_text;
use crate::visit::{self, VisitMut};

use uses::{shown, Access, Aliases, Collector, Place, Proj, Use};

use super::closure_names::Closures;

pub(crate) fn check_exclusive(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    // Which closure a local names, for the closures and functions used as values of the
    // program (`Checker::relaxed_callee`): built once, when the first of them is checked, while
    // every body is in place.
    let mut closures: Option<Closures> = None;
    for d in fns {
        if closures.is_none() && !cx.modules[cx.fn_info(d).module].is_std && as_value(cx, d) {
            closures = Some(Closures::of(cx));
        }
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        // By-reference `const`s that the rest of their block would invalidate become shares
        // first, so the call checks below see what lowering will do.
        let mut errors = vec![];
        let shared: Vec<bool> = f
            .body
            .locals
            .clone()
            .iter()
            .map(|l| cx.is_shared_value(l.ty))
            .collect();
        let aliases = aliasing_params(cx, d, &f);
        let held = crate::ownership::held_closures(cx, &f.body.block);
        let col = Collector {
            cx,
            locals: &f.body.locals,
            aliases: &aliases,
            held: &held,
        };
        let_borrow::check_body(
            &col,
            &f.body.locals,
            &shared,
            &mut f.body.block,
            &mut errors,
        );
        let aliases = aliasing_params(cx, d, &f);
        let params: Vec<(LocalId, LocalId)> = aliases.iter().map(|(l, p)| (*l, p.root)).collect();
        let soft: FxHashSet<Span> = cx.fn_info(d).soft_moves.iter().copied().collect();
        let mut checker = Checker {
            cx,
            locals: &f.body.locals,
            aliases,
            params,
            at: (d, &f.captures),
            closures: closures.as_ref(),
            held: &held,
            soft: &soft,
            errors: vec![],
            shares: vec![],
        };
        visit::block(&mut f.body.block, &mut checker);
        errors.extend(checker.errors);
        let shares = checker.shares;
        // A closure that modifies or takes over what it captured keeps its conflict: the call
        // would see it change under the borrow.
        let (mut kept, mut conflicts) = (vec![], vec![]);
        for (c, outer, err) in shares {
            match modifies_capture(cx, c, outer, 0) {
                true => conflicts.push(err),
                false => kept.push((c, outer)),
            }
        }
        errors.extend(conflicts);
        let shares = kept;
        cx.diags.extend(errors);
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        for (c, outer) in shares {
            share_capture(cx, c, outer);
        }
    }
}

/// Does closure `c` modify or take over what it captured of enclosing variable `outer`? Through
/// the captured name or any name for (a part of) it: a binding initialized from it (`const a =
/// cap`, `const row = cap[0]`, destructuring), the elements of a loop or the bindings of a
/// `match` over it (unless each is an object of its own: a class instance, an interface or
/// `any` value), a share of it passed anywhere, or a closure it creates. Unknown cases count as
/// modifying.
fn modifies_capture(cx: &Ctx, c: DefId, outer: LocalId, depth: u32) -> bool {
    let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] else {
        return true;
    };
    let Some(cap) = f.captures.iter().find(|k| k.outer == outer) else {
        return false;
    };
    if depth > 8 {
        return true;
    }
    let mut block = f.body.block.clone();
    // The names for the capture, until no binding adds one.
    let mut aliases = Aliasing {
        cx,
        f,
        names: FxHashSet::from_iter([cap.inner]),
        inits: FxHashSet::default(),
        grew: false,
    };
    loop {
        aliases.grew = false;
        visit::block(&mut block, &mut aliases);
        if !aliases.grew {
            break;
        }
    }
    if aliases
        .names
        .iter()
        .any(|l| f.body.locals[l.0 as usize].mutable)
    {
        return true;
    }
    let mut scan = Changes {
        cx,
        names: &aliases.names,
        inits: &aliases.inits,
        depth,
        hit: false,
    };
    visit::block(&mut block, &mut scan);
    scan.hit
}

/// The local at the root of place `e`, through shares and conversions (`None` for a value that
/// is not a place, or a copy: `clone()`).
fn alias_root(e: &Expr) -> Option<LocalId> {
    let mut x = e;
    loop {
        match &x.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => x = &args[0],
            E::Upcast(inner) | E::Downcast(inner) | E::WrapSome(inner) | E::Cast(inner) => {
                x = inner
            }
            E::Field { base, .. }
            | E::Index { base, .. }
            | E::UnwrapSome(base, _)
            | E::UnwrapVariant { expr: base, .. } => x = base,
            E::Local(l, _) => return Some(*l),
            _ => return None,
        }
    }
}

/// Collects the names for a capture (`modifies_capture`).
struct Aliasing<'a, 'm> {
    cx: &'a Ctx<'m>,
    f: &'a FnDef,
    names: FxHashSet<LocalId>,
    /// Spans of the initializers that bind a name (a share there is that name's).
    inits: FxHashSet<Span>,
    grew: bool,
}

impl Aliasing<'_, '_> {
    fn from(&mut self, init: &Expr) -> bool {
        alias_root(init).is_some_and(|l| self.names.contains(&l))
    }

    fn bind(&mut self, l: LocalId) {
        let ty = self.f.body.locals[l.0 as usize].ty;
        let own = self.cx.class_of(ty).is_some() || matches!(self.cx.ty.kind(ty), TyKind::Dyn(..));
        if !own && self.names.insert(l) {
            self.grew = true;
        }
    }

    fn bind_pat(&mut self, p: &Pat) {
        let mut ls = vec![];
        pat_locals(p, &mut ls);
        for l in ls {
            self.bind(l);
        }
    }
}

impl VisitMut for Aliasing<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } if self.from(init) => {
                self.inits.insert(init.span);
                self.bind(*local);
            }
            S::LetPat { pat, init } if self.from(init) => {
                self.inits.insert(init.span);
                self.bind_pat(pat);
            }
            S::ForOf { binding, iter, .. } if self.from(iter) => self.bind_pat(binding),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let E::Match { scrutinee, arms } = &e.kind {
            if self.from(scrutinee) {
                for a in arms {
                    self.bind_pat(&a.pat);
                }
            }
        }
    }
}

/// Finds a change through one of the names (`modifies_capture`).
struct Changes<'a, 'm> {
    cx: &'a Ctx<'m>,
    names: &'a FxHashSet<LocalId>,
    inits: &'a FxHashSet<Span>,
    depth: u32,
    hit: bool,
}

impl VisitMut for Changes<'_, '_> {
    fn expr(&mut self, e: &mut Expr) {
        let named = |x: &Expr| alias_root(x).is_some_and(|l| self.names.contains(&l));
        match &e.kind {
            E::Local(l, UseMode::BorrowMut | UseMode::Move) if self.names.contains(l) => {
                self.hit = true
            }
            E::Field {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            }
            | E::Index {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            }
            | E::UnwrapSome(_, UseMode::BorrowMut | UseMode::Move)
            | E::UnwrapVariant {
                mode: UseMode::BorrowMut | UseMode::Move,
                ..
            } if named(e) => self.hit = true,
            // Another reference to it, kept anywhere but a name the scan follows.
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                ..
            } if named(e) && !self.inits.contains(&e.span) => self.hit = true,
            E::Closure(nc) => {
                for &l in self.names.iter() {
                    if modifies_capture(self.cx, *nc, l, self.depth + 1) {
                        self.hit = true;
                    }
                }
            }
            _ => {}
        }
    }
}

/// The variables pattern `p` binds.
fn pat_locals(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, _) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|p| pat_locals(p, out))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|p| pat_locals(p, out)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| pat_locals(p, out)),
        PatKind::Some(p) => pat_locals(p, out),
        _ => {}
    }
}

/// Closure `c` shares enclosing variable `outer` instead of taking it over.
fn share_capture(cx: &mut Ctx, c: DefId, outer: LocalId) {
    if let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] {
        for cap in f.captures.iter_mut().filter(|cap| cap.outer == outer) {
            cap.share = true;
        }
    }
}

/// Closures and functions used as values: their non-Copy declared params may alias each other,
/// so they all stand for the first one.
fn aliasing_params(cx: &mut Ctx, d: DefId, f: &FnDef) -> Aliases {
    let mut out = Aliases::default();
    if !as_value(cx, d) {
        return out;
    }
    for (l, root) in alias_groups(cx, f) {
        out.insert(l, Place { root, proj: vec![] });
    }
    out
}

/// Is `d` a closure or a function used as a value?
fn as_value(cx: &Ctx, d: DefId) -> bool {
    cx.fn_info(d).kind == FnKind::Closure || cx.fn_values.iter().any(|(v, ..)| *v == d)
}

/// The non-Copy declared params of `f`, each with the first param it may be the same value as
/// (itself if none).
fn alias_groups(cx: &mut Ctx, f: &FnDef) -> FxHashMap<LocalId, LocalId> {
    let mut out = FxHashMap::default();
    let first = f.captures.len() + usize::from(f.self_ty.is_some());
    // Params that may be the same value (directly or through another, `any`) are one place.
    let params: Vec<(LocalId, crate::hir::TyId)> = f.params[first..]
        .iter()
        .filter(|p| !cx.is_copy(p.ty))
        .map(|p| (p.local, p.ty))
        .collect();
    let mut set: Vec<usize> = (0..params.len()).collect();
    fn find(set: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while set[r] != r {
            r = set[r];
        }
        set[i] = r;
        r
    }
    for i in 0..params.len() {
        for j in 0..i {
            if may_be_same_value(cx, params[i].1, params[j].1) {
                let (ri, rj) = (find(&mut set, i), find(&mut set, j));
                set[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    for i in 0..params.len() {
        let root = params[find(&mut set, i)].0;
        out.insert(params[i].0, root);
    }
    out
}

/// Can one value be of type `a` and of type `b` at once (so params of those types may be the
/// same object)? Not instances of two classes neither of which extends the other, nor an array
/// and a class instance; anything else (object types, which are structural, interfaces, `any`,
/// type parameters, unions) may be.
fn may_be_same_value(cx: &Ctx, a: crate::hir::TyId, b: crate::hir::TyId) -> bool {
    if a == b {
        return true;
    }
    let class = |t: crate::hir::TyId| cx.class_of(t).map(|(d, _)| d);
    match (class(a), class(b)) {
        (Some(x), Some(y)) => cx.class_extends(x, y) || cx.class_extends(y, x),
        (Some(_), None) => !matches!(cx.ty.kind(b), TyKind::Array(_)),
        (None, Some(_)) => !matches!(cx.ty.kind(a), TyKind::Array(_)),
        (None, None) => true,
    }
}

struct Checker<'a, 'm> {
    cx: &'a Ctx<'m>,
    locals: &'a [LocalDef],
    aliases: Aliases,
    /// The params `aliases` makes one place ([`aliasing_params`]), each with that place.
    params: Vec<(LocalId, LocalId)>,
    /// The function checked, and its captures.
    at: (DefId, &'a [Capture]),
    /// Which closure a local names; `None` before a program's closure is checked.
    closures: Option<&'a Closures>,
    /// Non-escaping closures held in a local (`crate::ownership::held_closures`).
    held: &'a FxHashMap<LocalId, DefId>,
    /// The function's soft moves (`FnInfo::soft_moves`).
    soft: &'a FxHashSet<Span>,
    errors: Vec<Diagnostic>,
    /// Captures that may share their variable ([`share_borrowed_captures`]), with the error
    /// to report if the closure modifies it.
    shares: Vec<(DefId, LocalId, Diagnostic)>,
}

/// The argument uses of one call: `(direct, nested)` per argument.
type ArgUses = Vec<(Vec<Use>, Vec<Use>)>;

impl Checker<'_, '_> {
    fn collector(&self) -> Collector<'_, '_> {
        Collector {
            cx: self.cx,
            locals: self.locals,
            aliases: &self.aliases,
            held: self.held,
        }
    }

    /// Borrowing bindings of `pat` point into `place`.
    fn bind(&mut self, pat: &Pat, place: &Place) {
        match &pat.kind {
            PatKind::Binding(l, UseMode::Borrow | UseMode::BorrowMut) => {
                self.aliases.insert(*l, place.clone());
            }
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|p| self.bind(p, place))
            }
            PatKind::Array { elems, .. } => elems.iter().for_each(|p| self.bind(p, place)),
            PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| self.bind(p, place)),
            PatKind::Some(p) => self.bind(p, place),
            _ => {}
        }
    }

    /// `held`: the call runs a non-escaping closure held in a local, whose borrowed captures
    /// are passed along with the arguments (the first entry of the uses).
    ///
    /// `key_only`: the call is a collection's `has`, `get` or `delete`, whose key argument (the
    /// last) the callee only compares by identity ([`identity_key_call`]).
    fn check_call(
        &mut self,
        args: &mut [Expr],
        mutex_with: bool,
        held: Option<(DefId, Span)>,
        key_only: bool,
        callee: Option<&Callee>,
    ) {
        let col = self.collector();
        let mut uses: ArgUses = vec![];
        if let Some((c, span)) = held {
            let mut caps = vec![];
            col.captures(c, span, false, &mut caps);
            uses.push((caps, vec![]));
        }
        if args.len() + uses.len() < 2 {
            return;
        }
        for a in args.iter_mut() {
            let (mut direct, mut nested) = (vec![], vec![]);
            col.direct(a, &mut direct, &mut nested);
            uses.push((direct, nested));
        }
        // `m.with(f)`: the callback gets the lock, i.e. mutable access to `m`'s value. Behind a
        // `shared` handle that value is not part of `m`'s place: a closure that only reads what
        // it captured (`this.step` next to `this.m`) reaches it only through the lock again,
        // and one that may change or move the handle still conflicts with the borrow (#459).
        let behind_shared = matches!(self.cx.ty.kind(args[0].ty), TyKind::Shared(_));
        if mutex_with && !behind_shared {
            let first = usize::from(held.is_some());
            for u in &mut uses[first].0 {
                u.access = Access::Unique;
            }
        }
        if key_only && held.is_none() && uses.len() == 2 {
            drop_key_through_receiver(&mut uses);
        }
        let shared = share_borrowed_captures(&mut uses);
        // Two whole params passed on to a callee checked with its params as one place
        // (`relaxed_callee`) are what it was checked for. Only such a pair of direct uses: a
        // nested use (a param changed while another argument is evaluated) keeps its conflict.
        let whole = |u: &Use| {
            u.place.proj.is_empty()
                && !u.captured
                && (self.params.iter()).any(|(l, root)| {
                    *root == u.place.root && self.locals[l.0 as usize].name == u.text
                })
        };
        let relaxed = std::cell::OnceCell::new();
        let skip = |u: &Use, w: &Use| {
            whole(u)
                && whole(w)
                && u.text != w.text
                && *relaxed.get_or_init(|| callee.is_some_and(|c| self.relaxed_callee(c)))
        };
        if let Some(d) = first_conflict(&uses, &skip) {
            self.errors.push(d);
        } else if !shared.is_empty() {
            // The conflict the shares resolve, for a closure that turns out to modify.
            for &(i, k, ..) in &shared {
                uses[i].0[k].access = Access::Move;
            }
            if let Some(err) = first_conflict(&uses, &skip) {
                for (_, _, c, outer) in shared {
                    self.shares.push((c, outer, err.clone()));
                }
            }
        }
    }
}

impl Checker<'_, '_> {
    /// Is `callee` a function checked as if its params may be one value, which assumes no
    /// exclusive access for any of them (a closure, or a function used as a value, whose
    /// modified params are `Borrow`, `finish::demote_fn_values`)? Two whole params of this
    /// function that may be one value (`mount(vnode, container)` passing both on, in a renderer
    /// whose nested functions are closures) are then what that callee was checked for (#1383),
    /// as in JavaScript, where they are one object. Not a part of one with the other (`(a.log,
    /// b)`: the callee was checked with an array and an object it can't contain), nor a param
    /// changed while another argument is evaluated, nor a named function that takes one of them
    /// `BorrowMut`: its body assumes they are not one value.
    fn relaxed_callee(&self, callee: &Callee) -> bool {
        if self.params.is_empty() {
            return false;
        }
        let h = match callee {
            Callee::Def(h, _) => *h,
            Callee::Indirect(c) => match c.kind {
                E::Local(l, _) => {
                    let cx = self.cx;
                    let caps_of = |g: DefId| -> &[Capture] {
                        match &cx.defs[g.0 as usize] {
                            Some(Def::Fn(p)) => &p.captures,
                            _ => &[],
                        }
                    };
                    let Some(closures) = self.closures else {
                        return false;
                    };
                    match closures.resolve(self.at.0, self.at.1, l, caps_of) {
                        Some(h) => h,
                        None => return false,
                    }
                }
                _ => return false,
            },
            _ => return false,
        };
        let info = self.cx.fn_info(h);
        let loose = info.kind == FnKind::Closure || self.cx.fn_values.iter().any(|(v, ..)| *v == h);
        loose && info.this.is_none() && info.params.iter().all(|p| p.mode != PassMode::BorrowMut)
    }
}

/// Is `callee` a `has`, `get` or `delete` of `Map`, `Set`, `WeakMap` or `WeakSet`? Their key
/// is compared by identity when it is an object (a number, `bool` or string is copied, so it is
/// no use here): the callee reads its address, never what it points to.
fn identity_key_call(cx: &Ctx, callee: &Callee) -> bool {
    const METHODS: [&str; 4] = [
        "std/prelude/map::Map.",
        "std/collections/set::Set.",
        "std/prelude/weak::WeakMap.",
        "std/prelude/weak::WeakSet.",
    ];
    let Callee::Def(d, _) = callee else {
        return false;
    };
    let Some(Def::Fn(f)) = &cx.defs[d.0 as usize] else {
        return false;
    };
    METHODS.iter().any(|m| {
        f.name
            .strip_prefix(m)
            .is_some_and(|n| matches!(n, "has" | "get" | "delete"))
    })
}

/// Is a key of type `t` compared by identity (a class instance, an object, an interface value,
/// an array or a function)? Not a value `struct`, whose fields are compared, nor a tuple.
fn by_identity(cx: &Ctx, t: crate::hir::TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => cx.adt(*d).is_some_and(|a| match a.kind {
            crate::hir::AdtKind::Class | crate::hir::AdtKind::Anon => true,
            // A fields-only interface; not a Velt `struct`, a value type.
            crate::hir::AdtKind::Struct => a.decl.is_none(),
        }),
        TyKind::Dyn(..) | TyKind::Array(_) | TyKind::FnPtr { .. } => true,
        _ => false,
    }
}

/// `link.dep.subs.delete(effect)` with `link` an element of `effect.deps`: the receiver is
/// reached through the borrowed key, but the callee reads only the key's address, so the
/// key's borrow of the object (a strict prefix of the receiver's place) is no use of the
/// receiver. A key inside the receiver (`for (const x of s) s.delete(x)`) keeps its use.
fn drop_key_through_receiver(uses: &mut ArgUses) {
    let receiver: Vec<Place> = uses[0].0.iter().map(|u| u.place.clone()).collect();
    uses[1].0.retain(|k| {
        let through = receiver
            .iter()
            .any(|r| k.place.proj.len() < r.proj.len() && k.place.overlaps(r));
        !(k.access == Access::Shared && k.capture.is_none() && through)
    });
}

/// A closure argument that takes over a variable another argument of the call only borrows
/// (`derive(parts, () => parts.length)`) shares it instead, as JavaScript refers to one value
/// from both: the call's borrow stays valid, and the variable stays the caller's. Not when an
/// argument may modify the variable, or takes it over too. Returns the captures to share, with
/// where their uses are (argument, index).
fn share_borrowed_captures(uses: &mut ArgUses) -> Vec<(usize, usize, DefId, LocalId)> {
    let mut out = vec![];
    for i in 0..uses.len() {
        for k in 0..uses[i].0.len() {
            let u = &uses[i].0[k];
            let Some((c, outer)) = u.capture else {
                continue;
            };
            if u.access != Access::Move || !u.place.proj.is_empty() || u.place.root != outer {
                continue;
            }
            let others = (uses.iter().enumerate())
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, (direct, nested))| direct.iter().chain(nested))
                .filter(|w| w.place.overlaps(&u.place));
            let mut borrowed = false;
            let mut only_borrows = true;
            for w in others {
                borrowed = true;
                only_borrows &= w.access == Access::Shared;
            }
            if borrowed && only_borrows {
                uses[i].0[k].access = Access::Shared;
                out.push((i, k, c, outer));
            }
        }
    }
    out
}

/// `skip`: a pair of direct uses that is no conflict.
fn first_conflict(uses: &ArgUses, skip: &dyn Fn(&Use, &Use) -> bool) -> Option<Diagnostic> {
    for (i, (direct_i, _)) in uses.iter().enumerate() {
        for (j, (direct_j, nested_j)) in uses.iter().enumerate() {
            if i == j {
                continue;
            }
            for u in direct_i {
                if i < j {
                    let hit = direct_j.iter().find(|w| clash(u, w) && !skip(u, w));
                    if let Some(w) = hit {
                        return Some(report_direct(u, w));
                    }
                }
                let by_ref = matches!(u.access, Access::Shared | Access::Unique);
                let hit = nested_j
                    .iter()
                    .find(|w| by_ref && u.place.overlaps(&w.place) && !changes_inside(u, w));
                if let Some(w) = hit {
                    return Some(report(u, w));
                }
            }
        }
    }
    None
}

/// Does `w`, a change made while another argument is evaluated, only change the inside of
/// the value `u` hands to the callee by reference (`this.set(this.src.get(1))`, `get`
/// modifying `this.src`)? JavaScript's callee then sees the change too: no conflict. Replacing
/// the value or a place on its way (`f(x.arr, reset(x))`) still is one.
fn changes_inside(u: &Use, w: &Use) -> bool {
    u.identity && w.access == Access::Unique && w.place.proj.len() > u.place.proj.len()
}

/// Whether Velt, as JavaScript, passes values of type `t` by reference: class instances and
/// arrays. An object type (`{ x: number }`) or a `struct` is inline: the call gets a copy made
/// when its argument is evaluated, so a later argument's change to it would not reach the
/// callee, as it does in JavaScript.
pub(super) fn by_reference(cx: &Ctx, t: crate::hir::TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => cx
            .adt(*d)
            .is_some_and(|a| a.kind == crate::hir::AdtKind::Class),
        TyKind::Array(_) => true,
        _ => false,
    }
}

/// Do two argument uses (`u` evaluated before `w`) conflict? Two moves, or a move followed
/// by a use, are left to the move analysis ("use of moved value").
fn clash(u: &Use, w: &Use) -> bool {
    if u.access == Access::Move || !u.place.overlaps(&w.place) {
        return false;
    }
    u.access == Access::Unique || w.access != Access::Shared
}

/// The mutable borrow is reported as the one the call holds, the other use as the error.
fn report_direct(u: &Use, w: &Use) -> Diagnostic {
    if w.access == Access::Unique && u.access != Access::Unique {
        report(w, u)
    } else {
        report(u, w)
    }
}

/// `held` is what the call keeps borrowed (label), `other` the conflicting use (primary span).
fn report(held: &Use, other: &Use) -> Diagnostic {
    let it = if held.text == other.text {
        "it".to_string()
    } else {
        shown(&held.text)
    };
    let name = shown(&other.text);
    let msg = match (held.access, other.access) {
        (Access::Unique, _) => {
            format!("cannot use {name} here: this call may modify {it} through another argument")
        }
        (_, Access::Move) => {
            format!("cannot move {name} here: {it} is already borrowed by this call")
        }
        _ => format!("cannot modify {name} here: {it} is already borrowed by this call"),
    };
    let label = match (held.access, held.captured) {
        (Access::Unique, true) => format!("{} is modified by this closure", shown(&held.text)),
        (Access::Unique, false) => "may be modified through this argument".to_string(),
        (_, true) => format!("{} is captured by this closure", shown(&held.text)),
        (_, false) => "borrowed here".to_string(),
    };
    Diagnostic::error(msg, other.span)
        .with_label(held.span, label)
        .with_note(
            "a value the callee may modify must not be reachable through another argument \
             of the same call; use a separate variable or `.clone()`",
        )
}

impl VisitMut for Checker<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            S::ForOf {
                binding,
                iter,
                body,
                consume,
                share_elems,
                ..
            } => {
                if let Some((mut p, _)) = self.collector().place_of(iter) {
                    if !*consume {
                        let text = place_text(self.cx, self.locals, iter);
                        let col = self.collector();
                        iteration::share_moved_iterated(&col, &p, self.soft, body);
                        let uses = iteration::body_uses(&col, body);
                        let hit = iteration::modified_while_iterating(&uses, &p, &text, iter.span);
                        self.errors.extend(hit);
                        *share_elems = iteration::replaces_bound(&uses, &p, binding);
                    }
                    p.proj.push(Proj::Index);
                    self.bind(binding, &p);
                }
            }
            S::LetPat { pat, init } => {
                if let Some((p, _)) = self.collector().place_of(init) {
                    self.bind(pat, &p);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            E::Match { scrutinee, arms } => {
                if let Some((p, _)) = self.collector().place_of(scrutinee) {
                    for a in arms.iter() {
                        self.bind(&a.pat, &p);
                    }
                }
            }
            E::Call { callee, args } => {
                let mutex_with = matches!(
                    callee,
                    Callee::Intrinsic(Intrinsic::MutexWith | Intrinsic::LockRead)
                );
                let held = self.collector().held_call(callee).map(|c| (c, e.span));
                let key_only = args.len() == 2
                    && by_identity(self.cx, args[1].ty)
                    && identity_key_call(self.cx, callee);
                self.check_call(args, mutex_with, held, key_only, Some(callee));
            }
            E::New { args, .. } => self.check_call(args, false, None, false, None),
            _ => {}
        }
    }
}
