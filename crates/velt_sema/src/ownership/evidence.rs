//! Evidence of mutation in one checked body (for mutation inference, `super::mutation`):
//! which locals have their contents modified and which are assigned as a whole.
//!
//! - A place used `BorrowMut` (written through, or passed to a param / receiver that is
//!   modified) modifies the local it is rooted at; so does a closure capturing the local
//!   mutably.
//! - `x = v` / `x += v` on a local reassigns it (a param reassigned as a whole is a local
//!   rebinding, JS semantics); it does not modify the caller's value.
//! - Borrowing pattern bindings (`for...of` elements, `match` / destructuring bindings) point into
//!   the matched place: modifying the binding modifies the local that place is rooted at.
//! - A call through a function value may modify what its non-Copy arguments point to. This is
//!   *weak* evidence: an argument that lies in its root's own memory (no array element on the
//!   way, e.g. `p` or `p.inner`) marks the root as possibly written, so the root is never
//!   assumed read-only (`LocalDef::mutable`), but it does not make the root a modified param for
//!   callers: the call site that supplies the function value accounts for what it modifies
//!   (`super::patch`, callback rule). Array elements live in the array's heap buffer, not in the
//!   root. A closure capturing a variable weakly passes that on to the enclosing function.
//!
//! Evidence is flow-insensitive: a mutation on any path (one branch, a loop body, a closure)
//! counts.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::body::places::place_root;
use crate::ctx::Ctx;
use crate::hir::{
    Block, Callee, Def, Expr, ExprKind as E, FnDef, LocalId, PassMode, Pat, PatKind, Stmt,
    StmtKind as S, UseMode,
};
use crate::visit::{self, VisitMut};

/// What one body does to its locals.
#[derive(Default)]
pub(super) struct Evidence {
    /// Locals whose contents are modified in place.
    pub mutated: FxHashSet<LocalId>,
    /// Locals whose memory a function value may write (weak evidence, see the module docs).
    pub weak: FxHashSet<LocalId>,
    /// Locals assigned as a whole, with the first such assignment.
    pub reassigned: FxHashMap<LocalId, Span>,
    /// Locals whose contents a callee modifies through a parameter it owns (a method returning
    /// the `this` it changes): no mode change, but the body modifies them too
    /// (`FnInfo::modified_params`).
    pub modified: FxHashSet<LocalId>,
    /// Locals changed through one of their parts (`o.f.x = 1`) or given to a function that
    /// may change them (`derived`'s held values), where a program reads a `shared`.
    pub deep: FxHashSet<LocalId>,
}

/// Is local `l` modified before the body rebinds it for good? The rebinding is the first
/// top-level `l = v` statement (runs unconditionally); `None` without one. Modifications after
/// it only touch the callee's own value.
pub(super) fn modified_before_rebind(cx: &mut Ctx, b: &Block, l: LocalId) -> Option<bool> {
    let at = b.stmts.iter().position(|s| match &s.kind {
        S::Expr(Expr {
            kind: E::Assign { place, .. },
            ..
        }) => matches!(place.kind, E::Local(x, _) if x == l),
        _ => false,
    })?;
    let mut prefix = Block {
        stmts: b.stmts[..=at].to_vec(),
        value: None,
        span: b.span,
    };
    Some(collect(cx, &mut prefix).mutated.contains(&l))
}

/// The evidence of `f`'s body, where a program reads a `shared` also through what `for…of`
/// loops walk (`Ctx::loop_roots`): a loop's bindings are not bound into its source (a map's
/// cursor loop, the iterator protocol), so a change to an element (also through a closure, an
/// alias or an element of an element) counts as a change of the source's local, for
/// `FnInfo::modified_params`.
pub(super) fn collect_in(cx: &mut Ctx, d: crate::hir::DefId, f: &mut FnDef) -> Evidence {
    let (mut ev, aliases) = collect_aliased(cx, &mut f.body.block);
    if !cx.frozen_reads {
        return ev;
    }
    let mut edges = super::derived::edges(cx, f);
    // Std's own bodies change only what they write directly: its containers never change
    // what they hold or walk (`Map.set`'s value goes into an array the map then changes, an
    // iterator steps over its source).
    if cx.modules[cx.fn_info(d).module].is_std {
        edges.clear();
    }
    let mut changed: FxHashSet<LocalId> = ev.mutated.iter().copied().collect();
    changed.extend(ev.modified.iter().copied());
    changed.extend(ev.weak.iter().copied());
    let mut deep: FxHashSet<LocalId> = ev.deep.iter().copied().collect();
    // Bounded: each round adds a local.
    for _ in 0..=f.body.locals.len() {
        let mut grew = false;
        for e in &edges {
            let Some(x) = e.src else { continue };
            let through = match e.held {
                true => deep.contains(&e.bound),
                false => changed.contains(&e.bound),
            };
            if through {
                // A change to what was projected out of `x` (`q = a[0]; q.v = 1`) is a change
                // through `x`, of what it holds.
                ev.modified.insert(x);
                grew |= changed.insert(x);
                grew |= deep.insert(x);
            }
        }
        if !grew {
            break;
        }
        ev.modified = through_aliases(&ev.modified, &aliases, false);
        changed.extend(ev.modified.iter().copied());
    }
    ev
}

/// Collect the evidence of body `b`.
pub(super) fn collect(cx: &mut Ctx, b: &mut Block) -> Evidence {
    collect_aliased(cx, b).0
}

fn collect_aliased(cx: &mut Ctx, b: &mut Block) -> (Evidence, Aliases) {
    let closures = closure_locals(cx, b);
    let mut v = Collector {
        cx,
        ev: Evidence::default(),
        aliases: FxHashMap::default(),
        skip: FxHashSet::default(),
        closures,
    };
    visit::block(b, &mut v);
    let Collector {
        mut ev, aliases, ..
    } = v;
    ev.mutated = through_aliases(&ev.mutated, &aliases, false);
    ev.weak = through_aliases(&ev.weak, &aliases, true);
    if !ev.modified.is_empty() {
        ev.modified = through_aliases(&ev.modified, &aliases, false);
    }
    (ev, aliases)
}

/// `set` plus the roots its borrowing bindings point into (only through bindings into their
/// root's own memory when `in_memory`).
fn through_aliases(
    set: &FxHashSet<LocalId>,
    aliases: &Aliases,
    in_memory: bool,
) -> FxHashSet<LocalId> {
    let mut out = set.clone();
    for &start in set {
        let mut l = start;
        // Bounded: alias chains follow declaration order, so they cannot cycle.
        for _ in 0..aliases.len() {
            match aliases.get(&l) {
                Some(&(root, inside)) if root != l && (inside || !in_memory) => {
                    out.insert(root);
                    l = root;
                }
                _ => break,
            }
        }
    }
    out
}

/// Borrowing binding → (the local its matched place is rooted at, the place lies in that
/// local's own memory).
type Aliases = FxHashMap<LocalId, (LocalId, bool)>;

struct Collector<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    ev: Evidence,
    aliases: Aliases,
    /// Whole-local assignment targets (not modifications of the local's contents).
    skip: FxHashSet<*const Expr>,
    /// Locals holding a closure literal (`closure_locals`).
    closures: Closures,
}

impl Collector<'_, '_> {
    fn bind(&mut self, p: &Pat, scrutinee: &Expr) {
        let Some(root) = place_root(scrutinee) else {
            return;
        };
        let inside = in_root_memory(scrutinee);
        let mut bindings = vec![];
        borrowing_bindings(p, &mut bindings);
        for l in bindings {
            self.aliases.insert(l, (root, inside));
        }
    }

    /// Arguments of a call through a function value `callee` (see the module docs): one held
    /// in a closure local changes what that closure may change, any other anything it is given.
    fn indirect_args(&mut self, callee: &Expr, args: &[Expr]) {
        let bits = match &callee.kind {
            E::Local(l, _) => (self.closures.get(l)).map(|&c| closure_changes(self.cx, c)),
            _ => None,
        };
        for (i, a) in args.iter().enumerate() {
            let known_unchanged = bits.is_some_and(|b| i >= 64 || b & (1 << i) == 0);
            if known_unchanged || self.cx.is_copy(a.ty) || !in_root_memory(a) {
                continue;
            }
            if let Some(l) = place_root(a) {
                self.ev.weak.insert(l);
            }
        }
    }
}

/// Is place `e` part of its root local's own memory (no array element on the way)?
pub(super) fn in_root_memory(e: &Expr) -> bool {
    match &e.kind {
        E::Local(..) => true,
        E::Field { base, .. }
        | E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::Downcast(base) => in_root_memory(base),
        _ => false,
    }
}

fn borrowing_bindings(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, UseMode::Borrow | UseMode::BorrowMut) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|q| borrowing_bindings(q, out))
        }
        PatKind::Array { elems, .. } => elems.iter().for_each(|q| borrowing_bindings(q, out)),
        PatKind::Adt { fields } => fields.iter().for_each(|(_, q)| borrowing_bindings(q, out)),
        PatKind::Some(q) => borrowing_bindings(q, out),
        _ => {}
    }
}

impl VisitMut for Collector<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::ForOf {
                binding,
                iter,
                consume: false,
                ..
            } => self.bind(binding, iter),
            S::LetPat { pat, init } => self.bind(pat, init),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if self.skip.remove(&(e as *const Expr)) {
            return;
        }
        match &e.kind {
            E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
                if let E::Local(l, _) = place.kind {
                    self.ev.reassigned.entry(l).or_insert(e.span);
                    self.skip.insert(&**place as *const Expr);
                } else if self.cx.frozen_reads {
                    // `i.v = 1` on any local (an owned one too), `st.m.get(k)!.v = 1` through
                    // what a method of `st` returned.
                    // Through a part (`o.f.x = 1`, not `o.f = 1`): a change of what `o` holds.
                    // A slot of a new container (`xs.slice()[0] = x`) is no one else's.
                    let fresh_slot = matches!(&place.kind, E::Field { base, .. } | E::Index { base, .. } if super::derived::fresh(self.cx, base));
                    let r = match fresh_slot {
                        true => vec![],
                        false => super::derived::roots(self.cx, place).locals,
                    };
                    let own_slot = matches!(&place.kind, E::Field { base, .. } | E::Index { base, .. } if matches!(base.kind, E::Local(..)));
                    if !own_slot {
                        self.ev.deep.extend(r.iter().copied());
                    }
                    self.ev.modified.extend(r);
                    // The place's own node is assigned, not changed in place.
                    self.skip.insert(&**place as *const Expr);
                }
            }
            E::Field {
                mode: UseMode::BorrowMut,
                ..
            }
            | E::Index {
                mode: UseMode::BorrowMut,
                ..
            }
            | E::UnwrapSome(_, UseMode::BorrowMut)
                if self.cx.frozen_reads =>
            {
                // The object at the place changes in place (`o.tags.push(x)`): a change
                // through `o`, of what it holds.
                let r = super::derived::roots(self.cx, e).locals;
                self.ev.deep.extend(r.iter().copied());
                self.ev.modified.extend(r);
            }
            E::Local(l, UseMode::BorrowMut) => {
                self.ev.mutated.insert(*l);
            }
            E::Match { scrutinee, arms } => {
                for a in arms {
                    self.bind(&a.pat, scrutinee);
                }
            }
            E::Closure(def) => {
                // A closure assigning a variable it captures by reference reassigns it (a
                // borrowed parameter then owns its value: the old one may be freed).
                let by_ref: Vec<(LocalId, LocalId)> = match &self.cx.defs[def.0 as usize] {
                    Some(Def::Fn(c)) => (c.captures.iter())
                        .filter(|c| c.mode == PassMode::BorrowMut)
                        .map(|c| (c.outer, c.inner))
                        .collect(),
                    _ => vec![],
                };
                for (outer, inner) in by_ref {
                    if closure_reassigns(self.cx, *def, inner, 0) {
                        self.ev.reassigned.entry(outer).or_insert(e.span);
                    }
                }
                if let Some(Def::Fn(c)) = &self.cx.defs[def.0 as usize] {
                    for cap in &c.captures {
                        let written = c.body.locals[cap.inner.0 as usize].mutable;
                        match cap.mode {
                            PassMode::BorrowMut => self.ev.mutated.insert(cap.outer),
                            PassMode::Borrow if written => self.ev.weak.insert(cap.outer),
                            // A closure sharing an object it changes changes the outer
                            // variable's object too (`FnInfo::modified_params`).
                            PassMode::Owned if written && self.cx.frozen_reads => {
                                self.ev.modified.insert(cap.outer)
                            }
                            _ => false,
                        };
                    }
                }
            }
            E::Call {
                callee: Callee::Indirect(callee),
                args,
            } => self.indirect_args(callee, args),
            // Only for a program that reads a `shared` (`FnInfo::modified_params`). Virtual
            // calls don't reach a value read out of one (no class with subclasses is).
            E::Call {
                callee: Callee::Def(d, _),
                args,
            } if self.cx.frozen_reads => {
                let bits = self.cx.fn_info(*d).modified_params;
                let std = self.cx.modules[self.cx.fn_info(*d).module].is_std;
                // A callback changing what it is given (`xs.forEach((x) => { x.v = 1; })`)
                // changes what the call's other arguments hold (`[x].forEach(…)` changes `x`).
                if changing_callback(self.cx, &self.closures, *d, args) {
                    for a in
                        (args.iter()).filter(|a| callback(self.cx, &self.closures, a).is_none())
                    {
                        let mut r = super::derived::roots(self.cx, a).locals;
                        r.extend(super::derived::elem_roots(self.cx, a).locals);
                        self.ev.deep.extend(r.iter().copied());
                        self.ev.modified.extend(r);
                    }
                }
                for (i, a) in args.iter().enumerate().take(64) {
                    if bits & (1 << i) != 0 && !self.cx.is_copy(a.ty) {
                        if !std {
                            self.ev.deep.extend(place_root(a));
                        }
                        self.ev.modified.extend(place_root(a));
                    }
                    // Std's containers change their own slots, never what they hold.
                    if let (E::Local(l, UseMode::BorrowMut), false) = (&a.kind, std) {
                        self.ev.deep.insert(*l);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The locals of a body that only ever hold one closure literal (`const g = (x) => …`).
pub(super) type Closures = FxHashMap<LocalId, crate::hir::DefId>;

/// The `Closures` of body `b` (none in a program that reads no `shared`: only its rules ask).
/// A local assigned anywhere, also by a closure capturing it, holds other values too.
pub(super) fn closure_locals(cx: &Ctx, b: &mut Block) -> Closures {
    if !cx.frozen_reads {
        return Closures::default();
    }
    struct V<'a, 'm> {
        cx: &'a Ctx<'m>,
        found: Closures,
        assigned: FxHashSet<LocalId>,
        /// A nested closure being inferred (its def taken out): what it assigns is unknown.
        unknown: bool,
    }
    impl VisitMut for V<'_, '_> {
        fn stmt(&mut self, s: &mut Stmt) {
            if let S::Let {
                local,
                init: Some(init),
            } = &s.kind
            {
                let mut e = init;
                while let E::Upcast(x) | E::Cast(x) = &e.kind {
                    e = x;
                }
                if let E::Closure(c) = e.kind {
                    self.found.insert(*local, c);
                }
            }
        }
        fn expr(&mut self, e: &mut Expr) {
            match &e.kind {
                E::Assign { place, .. } => {
                    if let E::Local(l, _) = place.kind {
                        self.assigned.insert(l);
                    }
                }
                E::Closure(d) => {
                    // Any capture but a copy may become one by reference or in a shared cell
                    // (capture modes are inferred with this): the closure may assign it.
                    match &self.cx.defs[d.0 as usize] {
                        Some(Def::Fn(c)) => {
                            let assigns =
                                c.captures.iter().filter(|cap| cap.mode != PassMode::Copy);
                            self.assigned.extend(assigns.map(|cap| cap.outer));
                        }
                        _ => self.unknown = true,
                    }
                }
                _ => {}
            }
        }
    }
    let mut v = V {
        cx,
        found: FxHashMap::default(),
        assigned: FxHashSet::default(),
        unknown: false,
    };
    visit::block(b, &mut v);
    let V {
        mut found,
        assigned,
        unknown,
        ..
    } = v;
    found.retain(|l, _| !unknown && !assigned.contains(l));
    found
}

/// What closure `c` may change of its params (bit `k`: param `k`): what its body changes
/// (`FnInfo::modified_params`), and what it gives a function value to (weak evidence, which
/// sets `LocalDef::mutable`).
fn closure_changes(cx: &Ctx, c: crate::hir::DefId) -> u64 {
    let mut bits = cx.fn_info(c).modified_params;
    if let Some(Def::Fn(f)) = &cx.defs[c.0 as usize] {
        let params = f.params.iter().skip(f.captures.len()).take(64);
        for (k, p) in params.enumerate() {
            if f.body.locals[p.local.0 as usize].mutable {
                bits |= 1 << k;
            }
        }
    }
    bits
}

/// The closure argument `a` is (a literal, or a variable holding one).
pub(super) fn callback(cx: &Ctx, closures: &Closures, a: &Expr) -> Option<crate::hir::DefId> {
    callback_of(cx, closures, a).map(|(c, _)| c)
}

/// What a closure argument `a` may change of what it is given (`callback`): a literal what its
/// body changes, as a direct call would, a variable also what it gives a function value.
fn callback_changes(cx: &Ctx, closures: &Closures, a: &Expr) -> Option<u64> {
    callback_of(cx, closures, a).map(|(c, literal)| match literal {
        true => cx.fn_info(c).modified_params,
        false => closure_changes(cx, c),
    })
}

/// `callback`, and whether `a` is the closure literal itself.
fn callback_of(cx: &Ctx, closures: &Closures, a: &Expr) -> Option<(crate::hir::DefId, bool)> {
    let mut a = a;
    while let E::Upcast(x) | E::Cast(x) = &a.kind {
        a = x;
    }
    match (&a.kind, cx.ty.kind(a.ty)) {
        (E::Closure(c), _) => Some((*c, true)),
        (_, crate::hir::TyKind::Closure(c)) => Some((*c, false)),
        (E::Local(l, _), _) => closures.get(l).map(|&c| (c, false)),
        // An adapter to the parameters a std callback takes (`{ let f = g; (x, i) => f(x) }`):
        // the closure it calls, whose parameters come first in the same order.
        (E::Block(b), _) => match (&b.stmts[..], b.value.as_deref()) {
            (
                [Stmt {
                    kind:
                        S::Let {
                            init: Some(init), ..
                        },
                    ..
                }],
                Some(Expr {
                    kind: E::Closure(_),
                    ..
                }),
            ) => callback_of(cx, closures, init),
            _ => None,
        },
        _ => None,
    }
}

/// Does a closure argument of the call of `d` change what it is given: for std's `reduce`,
/// the element, not the accumulator?
pub(super) fn changing_callback(
    cx: &Ctx,
    closures: &Closures,
    d: crate::hir::DefId,
    args: &[Expr],
) -> bool {
    let name = &cx.fn_info(d).name;
    let elem = match name.starts_with("std/")
        && [".reduce", ".reduceRight"]
            .iter()
            .any(|m| name.ends_with(m))
    {
        true => 1 << 1,
        false => u64::MAX,
    };
    args.iter()
        .filter_map(|a| callback_changes(cx, closures, a))
        .any(|bits| bits & elem != 0)
}

/// Does closure `def` (or one nested in it, through captures by reference) assign its local
/// `l` as a whole?
fn closure_reassigns(cx: &mut Ctx, def: crate::hir::DefId, l: LocalId, depth: u32) -> bool {
    if depth > 16 {
        return true;
    }
    let Some(Def::Fn(mut f)) = cx.defs[def.0 as usize].take() else {
        return false;
    };
    let mut found = false;
    let mut nested = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
        E::Assign { place, .. } | E::CompoundAssign { place, .. } => {
            found |= matches!(place.kind, E::Local(x, _) if x == l);
        }
        E::Closure(d) => nested.push(*d),
        _ => {}
    });
    cx.defs[def.0 as usize] = Some(Def::Fn(f));
    found
        || nested.into_iter().any(|d| {
            let inner: Vec<LocalId> = match &cx.defs[d.0 as usize] {
                Some(Def::Fn(c)) => (c.captures.iter())
                    .filter(|c| c.outer == l && c.mode == PassMode::BorrowMut)
                    .map(|c| c.inner)
                    .collect(),
                _ => vec![],
            };
            inner
                .into_iter()
                .any(|i| closure_reassigns(cx, d, i, depth + 1))
        })
}
