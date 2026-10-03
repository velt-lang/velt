//! What the values of a `with` callback may reach: every local of the callback, and of the
//! closures it creates, gets a set of *regions* — `IN` (the locked value: the callback's
//! parameter), `OUT` (the callback's captured state). A local bound to an expression reaches
//! what the expression mentions; a fresh local (`const tmp = new T()`) reaches nothing until
//! something is stored into it, and then what was stored (a fixpoint over the bodies,
//! `super::stores`; flow-insensitive). A closure's captures reach what the captured variables
//! reach; the parameters of one passed to a call reach what the call's other arguments reach
//! (`v.items.forEach((x) => out.push(x))`), and those of any other closure reach both regions.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::hir::{Callee, DefId, Expr, ExprKind as E, FnDef, LocalId, Pat, PatKind};
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
    /// Set when [`Regions::add`] grew a set (the fixpoint goes on).
    pub(super) changed: bool,
}

impl Regions {
    /// The starting regions of callback `c` and the closures in `bodies` (`c`'s included).
    pub(super) fn new(c: DefId, bodies: &[(DefId, FnDef)]) -> Self {
        let mut r = Regions {
            bits: HashMap::new(),
            alias: HashMap::new(),
            homes: HashSet::new(),
            captured: HashMap::new(),
            params: HashMap::new(),
            changed: false,
        };
        let (parent, passed) = closure_sites(bodies);
        for (d, f) in bodies {
            let n = f.captures.len();
            let params: Vec<LocalId> = f.params[n..].iter().map(|p| p.local).collect();
            for k in &f.captures {
                if *d == c {
                    r.bits.insert((c, k.inner), OUT);
                    r.homes.insert((c, k.inner));
                } else if let Some(&p) = parent.get(d) {
                    r.alias.insert((*d, k.inner), (p, k.outer));
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

    /// Is local `l` of body `d` the locked value or a captured variable?
    pub(super) fn is_home(&self, d: DefId, l: LocalId) -> bool {
        self.homes.contains(&self.key((d, l)))
    }

    /// The regions a value of `e` (in body `d`) may reach.
    pub(super) fn mentions(&self, cx: &mut Ctx, d: DefId, e: &Expr) -> u8 {
        if cx.is_copy(e.ty) {
            return 0;
        }
        match &e.kind {
            E::Local(l, _) => self.get(d, *l),
            E::Closure(n) => {
                let outer = self.captured.get(n).map_or(&[][..], |v| v.as_slice());
                outer.iter().fold(0, |b, l| b | self.get(d, *l))
            }
            E::Lit(_) | E::Global(_) | E::FnRef(..) | E::Assign { .. } => 0,
            E::Block(b) => b.value.as_ref().map_or(0, |v| self.mentions(cx, d, v)),
            E::If { then, els, .. } => self.mentions(cx, d, then) | self.mentions(cx, d, els),
            E::Match { arms, .. } => arms
                .iter()
                .fold(0, |b, a| b | self.mentions(cx, d, &a.body)),
            _ => {
                let mut kids = vec![];
                children(e, &mut kids);
                kids.iter().fold(0, |b, k| b | self.mentions(cx, d, k))
            }
        }
    }
}

/// A store of a value reaching `b` into a place reaching `dest` crosses the boundary.
pub(super) fn crosses(dest: u8, b: u8) -> bool {
    (dest & IN != 0 && b & OUT != 0) || (dest & OUT != 0 && b & IN != 0)
}

/// For each closure of `bodies`: the body creating it, and whether it is passed to a call.
fn closure_sites(bodies: &[(DefId, FnDef)]) -> (HashMap<DefId, DefId>, HashSet<DefId>) {
    let mut parent: HashMap<DefId, DefId> = HashMap::new();
    let mut passed: HashSet<DefId> = HashSet::new();
    for (d, f) in bodies {
        let mut block = f.body.block.clone();
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| match &e.kind {
            E::Closure(n) => {
                parent.entry(*n).or_insert(*d);
            }
            E::Call { args, .. } => {
                for a in args {
                    if let E::Closure(n) = a.kind {
                        passed.insert(n);
                    }
                }
            }
            _ => {}
        });
    }
    (parent, passed)
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
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => {}
    }
}
