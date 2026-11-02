//! What the values of a `with` callback may reach: every local of the callback, and of the
//! closures it creates, gets a set of *regions* — `IN` (the locked value: the callback's
//! parameter), `OUT` (the callback's captured state). A local bound to an expression reaches
//! what the expression mentions; a fresh local (`const tmp = new T()`) reaches nothing until
//! something is stored into it, and then what was stored (a fixpoint over the bodies,
//! `super::stores`; flow-insensitive). A closure's captures reach what the captured variables
//! reach; the parameters of one passed to a call reach what the call's other arguments reach
//! (`v.items.forEach((x) => out.push(x))`), and those of any other closure reach both regions.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, LocalId, Pat, PatKind, TyId, TyKind};
use crate::visit;

/// May reach the locked value.
pub(super) const IN: u8 = 1;
/// May reach the callback's captured state.
pub(super) const OUT: u8 = 2;
/// May reach a variable the callback took along into the lock (`super::moves`): released under
/// the lock, so a part of it may go into the value as it is, but not into the outside state.
pub(super) const MOVED: u8 = 4;

type Key = (DefId, LocalId);

/// The regions of the locals of a callback and its closures.
pub(super) struct Regions {
    bits: FxHashMap<Key, u8>,
    /// A closure's capture local → the captured variable (in the creating body).
    alias: FxHashMap<Key, Key>,
    /// The callback's parameter and captures: storing into them is a store across the boundary
    /// (assigning any other local only rebinds it).
    homes: FxHashSet<Key>,
    /// Captured variables of each closure (in the creating body).
    captured: FxHashMap<DefId, Vec<LocalId>>,
    /// Parameters of each closure (after its captures).
    pub(super) params: FxHashMap<DefId, Vec<LocalId>>,
    /// Function values called or passed in the bodies that are resolved to closures among the
    /// bodies (their spans): their bodies are checked, so calls need not copy anything.
    pub(super) resolved: FxHashSet<Span>,
    /// The callback's captures: its capture local → the captured variable (in the function
    /// making the callback).
    callback_captures: FxHashMap<LocalId, LocalId>,
    /// The callback.
    pub(super) callback: DefId,
    /// The named functions among what each resolved function value may be.
    pub(super) named: FxHashMap<Span, Vec<DefId>>,
    /// Set when [`Regions::add`] grew a set (the fixpoint goes on).
    pub(super) changed: bool,
    /// The callback is an `RwLock`'s `read` callback (`Some(true)`: one nested in another's).
    pub(super) read: Option<bool>,
    /// The types the value a `read` callback reads may hold (`lock_types`).
    pub(super) lock_tys: FxHashSet<TyId>,
    /// One of `lock_tys` may hide a value of any type (`lock_types`).
    pub(super) lock_opaque: bool,
    /// Variables of a `read` callback's bodies holding a closure it made, stored into the
    /// caller's containers while still used (body, variable, store; `super::made_copied`).
    pub(super) made_stored: Vec<(DefId, LocalId, Span)>,
    /// Variables declared with a new object (`const h = new Holder()`, an object literal).
    pub(super) made_locals: FxHashSet<Key>,
    /// Variables bound to a place rooted in another variable of the bodies (`const d = c`,
    /// `const r = c.rows`) → those roots: what is stored into the first reaches the root too.
    pub(super) roots: FxHashMap<Key, Vec<Key>>,
    /// Variables of the bodies bound to a call's result (`const g = first(mine)`).
    pub(super) call_locals: FxHashSet<Key>,
    /// Stores of a variable of the bodies into the caller's containers (body, variable, where).
    pub(super) outed: Vec<(DefId, LocalId, Span)>,
    /// Stores of what may be the caller's objects into a variable of the bodies (body, variable,
    /// where): after the variable was stored into the caller's containers, an error.
    pub(super) inbound: Vec<(DefId, LocalId, Span)>,
}

impl Regions {
    /// The starting regions of callback `c` and the closures in `bodies` (`c`'s included).
    /// `moved`: the callback's captures that moved into the lock (`super::moves`).
    pub(super) fn new(c: DefId, bodies: &mut [(DefId, FnDef)], moved: &FxHashSet<LocalId>) -> Self {
        let mut r = Regions {
            bits: FxHashMap::default(),
            alias: FxHashMap::default(),
            homes: FxHashSet::default(),
            captured: FxHashMap::default(),
            params: FxHashMap::default(),
            resolved: FxHashSet::default(),
            named: FxHashMap::default(),
            callback_captures: FxHashMap::default(),
            callback: c,
            changed: false,
            read: None,
            lock_tys: FxHashSet::default(),
            lock_opaque: false,
            made_stored: vec![],
            made_locals: FxHashSet::default(),
            roots: FxHashMap::default(),
            call_locals: FxHashSet::default(),
            outed: vec![],
            inbound: vec![],
        };
        let Sites { parent, passed, .. } = closure_sites(bodies);
        for (d, f) in bodies.iter() {
            let n = f.captures.len();
            let params: Vec<LocalId> = f.params[n..].iter().map(|p| p.local).collect();
            for k in &f.captures {
                match parent.get(d) {
                    Some(&p) if *d != c => {
                        r.alias.insert((*d, k.inner), (p, k.outer));
                    }
                    // The callback's, or those of a closure made outside it that it calls
                    // (`super::values`): outside state.
                    _ => {
                        let region = match *d == c && moved.contains(&k.inner) {
                            true => MOVED,
                            false => OUT,
                        };
                        r.bits.insert((*d, k.inner), region);
                        r.homes.insert((*d, k.inner));
                    }
                }
            }
            if *d == c {
                for &p in &params {
                    r.bits.insert((c, p), IN);
                    r.homes.insert((c, p));
                }
            } else if !passed.contains(d) {
                // Parameters of a closure not passed to a call may be given anything.
                for &p in &params {
                    r.bits.insert((*d, p), IN | OUT);
                }
            }
            if *d == c {
                r.callback_captures = f.captures.iter().map(|k| (k.inner, k.outer)).collect();
            }
            r.captured
                .insert(*d, f.captures.iter().map(|k| k.outer).collect());
            r.params.insert(*d, params);
        }
        r
    }

    pub(super) fn key(&self, mut k: Key) -> Key {
        while let Some(&a) = self.alias.get(&k) {
            k = a;
        }
        k
    }

    /// The regions local `l` of body `d` may reach.
    pub(super) fn get(&self, d: DefId, l: LocalId) -> u8 {
        self.bits.get(&self.key((d, l))).copied().unwrap_or(0)
    }

    /// Local `l` of body `d` may reach `b` too.
    pub(super) fn add(&mut self, d: DefId, l: LocalId, b: u8) {
        let mut todo = vec![self.key((d, l))];
        let mut seen = FxHashSet::default();
        while let Some(k) = todo.pop() {
            if !seen.insert(k) {
                continue;
            }
            let e = self.bits.entry(k).or_insert(0);
            if *e | b != *e {
                *e |= b;
                self.changed = true;
            }
            // What a variable naming a part of another's holds, that one holds too.
            if let Some(rs) = self.roots.get(&k) {
                let rs = rs
                    .iter()
                    .map(|r| self.key(*r))
                    .filter(|r| !self.homes.contains(r));
                todo.extend(rs.collect::<Vec<Key>>());
            }
        }
    }

    /// Local `l` of body `d` and the variables it may name the object (or a part) of, through
    /// `link`s either way (`const d = c`): a store into one is a store into the others.
    pub(super) fn same_object(&self, d: DefId, l: LocalId) -> Vec<Key> {
        let mut out = vec![self.key((d, l))];
        let mut i = 0;
        while i < out.len() && out.len() < 64 {
            let k = out[i];
            let mut next: Vec<Key> = vec![];
            if let Some(rs) = self.roots.get(&k) {
                next.extend(rs.iter().map(|r| self.key(*r)));
            }
            for (from, rs) in &self.roots {
                if rs.iter().any(|r| self.key(*r) == k) {
                    next.push(self.key(*from));
                }
            }
            for n in next {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
            i += 1;
        }
        out
    }

    /// Local `l` of body `d` names a part of local `root` (of the same body).
    pub(super) fn link(&mut self, d: DefId, l: LocalId, root: LocalId) {
        let (k, r) = (self.key((d, l)), (d, root));
        let rs = self.roots.entry(k).or_default();
        if !rs.contains(&r) {
            rs.push(r);
            self.changed = true;
        }
    }

    /// The variable of the function making the callback that local `l` of body `d` is (a
    /// capture of the callback, or of a closure made in it), if any.
    pub(super) fn captured_var(&self, d: DefId, l: LocalId) -> Option<LocalId> {
        let (b, inner) = self.key((d, l));
        (b == self.callback)
            .then(|| self.callback_captures.get(&inner).copied())
            .flatten()
    }

    /// Is local `l` of body `d` the locked value or a captured variable?
    pub(super) fn is_home(&self, d: DefId, l: LocalId) -> bool {
        self.homes.contains(&self.key((d, l)))
    }

    /// The regions a value of `e` (in body `d`) may reach.
    pub(super) fn mentions(&self, cx: &mut Ctx, d: DefId, e: &Expr) -> u8 {
        let local = |l: LocalId| u64::from(self.get(d, l));
        let captured = |n: DefId| self.captured.get(&n).cloned().unwrap_or_default();
        reach(cx, e, &local, &captured) as u8
    }
}

/// What a value of `e` may reach: the union of `local(l)` over the locals it mentions (through
/// the captured variables of a closure it makes, `captured(n)`). Copy values and strings
/// (immutable, with atomic counts) reach nothing.
pub(super) fn reach(
    cx: &mut Ctx,
    e: &Expr,
    local: &dyn Fn(LocalId) -> u64,
    captured: &dyn Fn(DefId) -> Vec<LocalId>,
) -> u64 {
    if cx.is_copy(e.ty) || cx.is_string_value(e.ty) {
        return 0;
    }
    match &e.kind {
        E::Local(l, _) => local(*l),
        E::Closure(n) => captured(*n).iter().fold(0, |b, l| b | local(*l)),
        E::Lit(_) | E::Global(_) | E::FnRef(..) | E::Assign { .. } => 0,
        E::Block(b) => b
            .value
            .as_ref()
            .map_or(0, |v| reach(cx, v, local, captured)),
        E::If { then, els, .. } => {
            reach(cx, then, local, captured) | reach(cx, els, local, captured)
        }
        E::Match { arms, .. } => arms
            .iter()
            .fold(0, |b, a| b | reach(cx, &a.body, local, captured)),
        // A copy of anything (`x.clone()`, `structuredClone(x)`) is a new value holding nothing
        // of either side.
        E::Call {
            callee: Callee::Intrinsic(crate::hir::Intrinsic::Clone),
            ..
        } => 0,
        E::Call {
            callee: Callee::Def(g, _),
            ..
        } if Some(*g) == cx.prelude_fn("structuredClone") => 0,
        _ => {
            let mut kids = vec![];
            children(e, &mut kids);
            kids.iter()
                .fold(0, |b, k| b | reach(cx, k, local, captured))
        }
    }
}

/// The types a value of `t` may hold at any depth, `t` included: a union's members, and a
/// class's subclasses with the fields they add, and the types in a function type's signature,
/// count too. Returns whether one of them may hide a value of any type (an interface or `any`,
/// a type parameter, a closure's own type, a `shared` value, a generic subclass): then any type
/// may be a part of the value (`stores::read_part`).
pub(super) fn lock_types(cx: &mut Ctx, t: TyId, out: &mut FxHashSet<TyId>) -> bool {
    if out.contains(&t) {
        return false;
    }
    let opaque = matches!(
        cx.ty.kind(t),
        TyKind::Dyn(..)
            | TyKind::Param(_)
            | TyKind::Closure(_)
            | TyKind::Shared(_)
            | TyKind::Op(..)
    );
    if opaque {
        out.insert(t);
        return true;
    }
    if cx.is_copy(t) || cx.is_string_value(t) {
        return false;
    }
    if out.len() > 512 {
        return true;
    }
    out.insert(t);
    let mut opaque = false;
    let parts: Vec<TyId> = if let Some(ms) = cx.union_members(t) {
        ms
    } else {
        match cx.ty.kind(t).clone() {
            TyKind::Array(e) | TyKind::Option(e, _) => vec![e],
            TyKind::Tuple(ts) => ts,
            TyKind::Map(k, v) | TyKind::Promise(k, v) | TyKind::Result(k, v) => vec![k, v],
            // What a function value gives its caller has the types of its signature: what it
            // returns or throws, and what it passes to the functions it is given.
            TyKind::FnPtr {
                mut params,
                ret,
                throws,
                ..
            } => {
                params.extend([ret, throws]);
                params
            }
            TyKind::Adt(d, args) => {
                let mut parts: Vec<TyId> = if let Some(a) = cx.adt(d) {
                    a.fields.iter().map(|f| f.ty).collect()
                } else if let Some(en) = cx.enum_info(d) {
                    en.variants.iter().flat_map(|v| v.payload.clone()).collect()
                } else {
                    return true;
                };
                parts = parts.into_iter().map(|f| cx.subst(f, &args)).collect();
                parts.extend(args.iter().copied());
                // A value of a class may be one of its subclasses, holding the fields they add.
                if cx.class_of(t).is_some() {
                    for s in subclasses(cx, d) {
                        match cx.adt(s) {
                            Some(sa) if sa.generics.names.is_empty() => {
                                parts.push(cx.ty.intern(TyKind::Adt(s, vec![])));
                            }
                            _ => opaque = true,
                        }
                    }
                }
                parts
            }
            _ => vec![],
        }
    };
    for p in parts {
        opaque |= lock_types(cx, p, out);
    }
    opaque
}

/// Can a value of `t` hold a shared (non-Copy, non-string) value somewhere inside it? Only a
/// store into such a place can make a part of one side reachable from the other.
pub(super) fn holds_shared(cx: &mut Ctx, t: TyId) -> bool {
    holds_shared_in(cx, t, 0)
}

fn holds_shared_in(cx: &mut Ctx, t: TyId, depth: u32) -> bool {
    if depth > 8 || cx.is_copy(t) || cx.is_string_value(t) {
        return false;
    }
    let (parts, class): (Vec<TyId>, Option<DefId>) = match cx.ty.kind(t).clone() {
        TyKind::Array(e) | TyKind::Option(e, _) => (vec![e], None),
        TyKind::Tuple(ts) => (ts, None),
        TyKind::Adt(d, args) => {
            // A union (or another enum) may hold any of its members.
            let Some(a) = cx.adt(d) else {
                return true;
            };
            let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
            (
                fields.into_iter().map(|f| cx.subst(f, &args)).collect(),
                Some(d),
            )
        }
        TyKind::Promise(..) => return false,
        // Type parameters, interface and function values may hold anything.
        _ => return true,
    };
    let held = parts.into_iter().any(|p| part_holds_shared(cx, p, depth));
    // A value of a class may be one of its subclasses: the fields they add count too.
    held || class.is_some_and(|d| {
        subclasses(cx, d).into_iter().any(|s| {
            let own: Vec<TyId> = cx.adt(s).map_or(vec![], |a| {
                a.fields[a.own_fields_start..]
                    .iter()
                    .map(|f| f.ty)
                    .collect()
            });
            own.into_iter().any(|p| part_holds_shared(cx, p, depth))
        })
    })
}

fn part_holds_shared(cx: &mut Ctx, p: TyId, depth: u32) -> bool {
    (cx.is_shared_value(p) && !cx.is_string_value(p)) || holds_shared_in(cx, p, depth + 1)
}

/// The classes extending class `d`, directly or not.
pub(super) fn subclasses(cx: &mut Ctx, d: DefId) -> Vec<DefId> {
    if cx.subclasses.is_none() {
        let mut map: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        let base_of = |cx: &Ctx, c: DefId| {
            cx.adt(c)
                .and_then(|a| a.base)
                .and_then(|b| cx.class_of(b))
                .map(|(b, _)| b)
        };
        for s in (0..cx.info.len() as u32).map(DefId) {
            let mut base = base_of(cx, s);
            // Bounded like `Ctx::class_extends`.
            for _ in 0..64 {
                let Some(b) = base else { break };
                map.entry(b).or_default().push(s);
                base = base_of(cx, b);
            }
        }
        cx.subclasses = Some(map);
    }
    cx.subclasses
        .as_ref()
        .and_then(|m| m.get(&d))
        .cloned()
        .unwrap_or_default()
}

/// A store of a value reaching `b` into a place reaching `dest` crosses the boundary. A moved
/// variable is released under the lock, so it shares with the value freely; with the outside
/// state it shares nothing, either way: a part of it stored outside could also be in the value.
pub(super) fn crosses(dest: u8, b: u8) -> bool {
    (dest & (IN | MOVED) != 0 && b & OUT != 0) || (dest & OUT != 0 && b & (IN | MOVED) != 0)
}

/// Where the closures of the bodies are made: the creating body, and whether one is passed
/// to a call.
struct Sites {
    /// The body being visited.
    body: DefId,
    parent: FxHashMap<DefId, DefId>,
    passed: FxHashSet<DefId>,
}

impl visit::VisitMut for Sites {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(n) => {
                self.parent.entry(*n).or_insert(self.body);
            }
            E::Call { args, .. } => {
                for a in args {
                    if let E::Closure(n) = a.kind {
                        self.passed.insert(n);
                    }
                }
            }
            _ => {}
        }
    }
}

fn closure_sites(bodies: &mut [(DefId, FnDef)]) -> Sites {
    let mut sites = Sites {
        body: DefId(0),
        parent: FxHashMap::default(),
        passed: FxHashSet::default(),
    };
    for (d, f) in bodies.iter_mut() {
        sites.body = *d;
        visit::block(&mut f.body.block, &mut sites);
    }
    sites
}

/// The direct subexpressions of `e` whose values may become part of `e`'s value.
fn children<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    match &e.kind {
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::ToDyn { expr: x, .. }
        | E::Field { base: x, .. }
        | E::Index { base: x, .. } => out.push(x),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                out.push(c);
            }
            out.extend(args.iter());
        }
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => out.extend(xs.iter()),
        _ => {}
    }
}

/// The locals a pattern binds.
pub(super) fn pat_locals(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, _) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|x| pat_locals(x, out))
        }
        PatKind::Array { elems, rest } => {
            elems.iter().for_each(|x| pat_locals(x, out));
            out.extend(rest.iter().copied());
        }
        PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| pat_locals(x, out)),
        PatKind::Some(x) => pat_locals(x, out),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
    }
}
