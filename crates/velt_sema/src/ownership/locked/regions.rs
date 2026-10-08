//! What the values of a `with` callback may reach: every local of the callback, and of the
//! closures it creates, gets a set of *regions* — `IN` (the locked value: the callback's
//! parameter), `OUT` (the callback's captured state). A local bound to an expression reaches
//! what the expression mentions; a fresh local (`const tmp = new T()`) reaches nothing until
//! something is stored into it, and then what was stored (a fixpoint over the bodies,
//! `super::stores`; flow-insensitive). A closure's captures reach what the captured variables
//! reach; the parameters of one passed to a call reach what the call's other arguments reach
//! (`v.items.forEach((x) => out.push(x))`), and those of any other closure reach both regions.

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, LocalId, Pat, PatKind, TyId, TyKind};
use crate::visit;

/// May reach the locked value.
pub(super) const IN: u8 = 1;
/// May reach the callback's captured state.
pub(super) const OUT: u8 = 2;

type Key = (DefId, LocalId);

/// The regions of the locals of a callback and its closures.
pub(super) struct Regions {
    bits: HashMap<Key, u8>,
    /// A closure's capture local → the captured variable (in the creating body).
    alias: HashMap<Key, Key>,
    /// The callback's parameter and captures: storing into them is a store across the boundary
    /// (assigning any other local only rebinds it).
    homes: HashSet<Key>,
    /// Captured variables of each closure (in the creating body).
    captured: HashMap<DefId, Vec<LocalId>>,
    /// Parameters of each closure (after its captures).
    pub(super) params: HashMap<DefId, Vec<LocalId>>,
    /// Function values called or passed in the bodies that are resolved to closures among the
    /// bodies (their spans): their bodies are checked, so calls need not copy anything.
    pub(super) resolved: HashSet<Span>,
    /// The callback's captures: its capture local → the captured variable (in the function
    /// making the callback).
    callback_captures: HashMap<LocalId, LocalId>,
    /// The callback.
    callback: DefId,
    /// The named functions among what each resolved function value may be.
    pub(super) named: HashMap<Span, Vec<DefId>>,
    /// Set when [`Regions::add`] grew a set (the fixpoint goes on).
    pub(super) changed: bool,
}

impl Regions {
    /// The starting regions of callback `c` and the closures in `bodies` (`c`'s included).
    pub(super) fn new(c: DefId, bodies: &mut [(DefId, FnDef)]) -> Self {
        let mut r = Regions {
            bits: HashMap::new(),
            alias: HashMap::new(),
            homes: HashSet::new(),
            captured: HashMap::new(),
            params: HashMap::new(),
            resolved: HashSet::new(),
            named: HashMap::new(),
            callback_captures: HashMap::new(),
            callback: c,
            changed: false,
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
                        r.bits.insert((*d, k.inner), OUT);
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

    fn key(&self, mut k: Key) -> Key {
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
        let k = self.key((d, l));
        let e = self.bits.entry(k).or_insert(0);
        if *e | b != *e {
            *e |= b;
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
        _ => {
            let mut kids = vec![];
            children(e, &mut kids);
            kids.iter()
                .fold(0, |b, k| b | reach(cx, k, local, captured))
        }
    }
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
    let parts: Vec<TyId> = match cx.ty.kind(t).clone() {
        TyKind::Array(e) | TyKind::Option(e) => vec![e],
        TyKind::Tuple(ts) => ts,
        TyKind::Adt(d, args) => {
            let fields: Vec<TyId> = cx
                .adt(d)
                .map_or(vec![], |a| a.fields.iter().map(|f| f.ty).collect());
            fields.into_iter().map(|f| cx.subst(f, &args)).collect()
        }
        TyKind::Promise(..) => return false,
        // Type parameters, interface and function values may hold anything.
        _ => return true,
    };
    parts.into_iter().any(|p| {
        (cx.is_shared_value(p) && !cx.is_string_value(p)) || holds_shared_in(cx, p, depth + 1)
    })
}

/// A store of a value reaching `b` into a place reaching `dest` crosses the boundary.
pub(super) fn crosses(dest: u8, b: u8) -> bool {
    (dest & IN != 0 && b & OUT != 0) || (dest & OUT != 0 && b & IN != 0)
}

/// Where the closures of the bodies are made: the creating body, and whether one is passed
/// to a call.
struct Sites {
    /// The body being visited.
    body: DefId,
    parent: HashMap<DefId, DefId>,
    passed: HashSet<DefId>,
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
        parent: HashMap::new(),
        passed: HashSet::new(),
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
