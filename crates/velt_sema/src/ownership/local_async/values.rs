//! The program's function values by the types of their parameters, for
//! [`super::targets::value_targets`] (#854): so that a call's type tries only those that can accept
//! it, not all of them, and [`unify`], which decides.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};

/// What a parameter's type must be for a value of a type to be possibly accepted, without looking
/// further than its outermost type ([`unify`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Head {
    Adt(DefId),
    Dyn(DefId),
    Kind(std::mem::Discriminant<TyKind>),
}

/// A type without its `| null`.
pub(super) fn strip(cx: &Ctx, t: TyId) -> TyId {
    match cx.ty.kind(t) {
        TyKind::Option(inner, _) => *inner,
        _ => t,
    }
}

/// A type that unifies with every other: a type parameter, or a dynamic type.
fn is_open(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Param(_)) || cx.dynamic(t).is_some()
}

/// Open as `exact` matching ([`unify`]) counts a type a call gives: only a type parameter is.
fn open_as(cx: &Ctx, t: TyId, exact: bool) -> bool {
    match exact {
        true => matches!(cx.ty.kind(t), TyKind::Param(_)),
        false => is_open(cx, t),
    }
}

fn head(cx: &Ctx, t: TyId) -> Head {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => Head::Adt(*d),
        TyKind::Dyn(d, _) => Head::Dyn(*d),
        k => Head::Kind(std::mem::discriminant(k)),
    }
}

/// The program's function values of one arity. Each is listed once, under its first parameter
/// that is not open (or in `open`), by its position in [`Literals`]' values.
#[derive(Default)]
struct Arity {
    open: Vec<u32>,
    at: Vec<Position>,
}

/// The function values of an arity listed under a parameter position.
#[derive(Default)]
struct Position {
    all: Vec<u32>,
    by_head: FxHashMap<Head, Vec<u32>>,
    /// Those whose parameter is an interface, which a class or another type may implement.
    ifaces: Vec<u32>,
}

/// The program's function values, so that a call's type tries only those that can accept it, not
/// all of them.
#[derive(Default)]
pub(super) struct ValueIndex {
    list: Vec<(DefId, Vec<TyId>)>,
    has: rustc_hash::FxHashSet<DefId>,
    by_arity: FxHashMap<usize, Arity>,
    /// Matched as [`unify`]'s `exact`: a dynamic type is listed like any other, not as open.
    exact: bool,
}

impl ValueIndex {
    /// The function values by their own parameters.
    pub(super) fn of_values(cx: &Ctx, values: &[DefId]) -> ValueIndex {
        let mut out = ValueIndex::default();
        for &g in values {
            let Some(Def::Fn(f)) = &cx.defs[g.0 as usize] else {
                continue;
            };
            if out.has.insert(g) {
                let ps = f.params[f.captures.len()..].iter().map(|p| p.ty).collect();
                out.add(cx, g, ps);
            }
        }
        out
    }

    /// The function values by the parameters of the function type each is made as, each once per
    /// type.
    pub(super) fn of_types(cx: &Ctx, made: &[(DefId, TyId)]) -> ValueIndex {
        let mut out = ValueIndex {
            exact: true,
            ..ValueIndex::default()
        };
        let mut seen = rustc_hash::FxHashSet::default();
        for &(g, ty) in made {
            if !seen.insert((g, ty)) {
                continue;
            }
            let ps = match (cx.ty.kind(ty), &cx.defs[g.0 as usize]) {
                (TyKind::FnPtr { params, .. }, _) => params.clone(),
                (_, Some(Def::Fn(f))) => {
                    f.params[f.captures.len()..].iter().map(|p| p.ty).collect()
                }
                _ => continue,
            };
            out.has.insert(g);
            out.add(cx, g, ps);
        }
        out
    }

    fn add(&mut self, cx: &Ctx, g: DefId, ps: Vec<TyId>) {
        let i = self.list.len() as u32;
        let arity = self.by_arity.entry(ps.len()).or_default();
        arity.at.resize_with(ps.len(), Position::default);
        let first = (ps.iter().enumerate())
            .map(|(k, &p)| (k, strip(cx, p)))
            .find(|&(_, t)| !open_as(cx, t, self.exact));
        match first {
            None => arity.open.push(i),
            Some((k, t)) => {
                let at = &mut arity.at[k];
                at.all.push(i);
                let h = head(cx, t);
                at.by_head.entry(h).or_default().push(i);
                if matches!(h, Head::Dyn(_)) && cx.dynamic(t).is_none() {
                    at.ifaces.push(i);
                }
            }
        }
        self.list.push((g, ps));
    }

    /// Is `g` one of the function values?
    pub(super) fn has(&self, g: DefId) -> bool {
        self.has.contains(&g)
    }

    /// The function value at position `i` of [`Self::candidates`].
    pub(super) fn value(&self, i: u32) -> DefId {
        self.list[i as usize].0
    }

    /// The types of its parameters.
    pub(super) fn params(&self, i: u32) -> &[TyId] {
        &self.list[i as usize].1
    }

    /// The function values that may accept arguments of types `want`, in the order the program
    /// makes them; [`fits`] decides. `loose`: as [`fits`]'.
    pub(super) fn candidates(&self, cx: &Ctx, want: &[TyId], loose: bool) -> Vec<u32> {
        let mut out: Vec<u32> = vec![];
        let mut one = |arity: &Arity| {
            out.extend(&arity.open);
            for (k, at) in arity.at.iter().enumerate() {
                let Some(&w) = want.get(k) else {
                    // A parameter the call does not give: nothing says it does not fit.
                    out.extend(&at.all);
                    continue;
                };
                let w = strip(cx, w);
                if open_as(cx, w, self.exact) {
                    out.extend(&at.all);
                    continue;
                }
                let mut heads = |h: Head| out.extend(at.by_head.get(&h).into_iter().flatten());
                match head(cx, w) {
                    // Its own class, or one it extends.
                    Head::Adt(d) => {
                        let mut d = d;
                        for _ in 0..64 {
                            heads(Head::Adt(d));
                            let base = cx.adt(d).and_then(|a| a.base).map(|b| cx.ty.kind(b));
                            let Some(TyKind::Adt(b, _)) = base else { break };
                            d = *b;
                        }
                        out.extend(&at.ifaces);
                    }
                    Head::Dyn(i) => heads(Head::Dyn(i)),
                    h => {
                        heads(h);
                        out.extend(&at.ifaces);
                    }
                }
            }
        };
        match loose {
            true => self.by_arity.values().for_each(&mut one),
            false => self
                .by_arity
                .get(&want.len())
                .into_iter()
                .for_each(&mut one),
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Do parameters of types `ps` accept arguments of types `want`? `loose`: a call through a value
/// read from an `any` passes what it is given, so the numbers need not agree: a parameter it does
/// not give is left out, an argument with no parameter is dropped.
pub(super) fn fits(cx: &Ctx, ps: &[TyId], want: &[TyId], exact: bool, loose: bool) -> bool {
    (loose || ps.len() == want.len())
        && ps
            .iter()
            .zip(want)
            .all(|(&p, &w)| unify(cx, p, w, exact, 0))
}

/// Can a parameter of type `p` be given a value of type `a`? The types unify: the same type,
/// either open (a type parameter, `any`, `unknown`, `object`) at any depth (`T[]` and `string[]`,
/// `Map<K, V>` and `Map<string, App>`), an interface `a` implements, or a base class of `a`.
///
/// `exact`: `p` is a parameter of the type a value is made as and `a` of the type it is read as. A
/// dynamic type is then not open on either side, it unifies only with itself: a value made as
/// `(h: HttpRequest) => …` is not in a place of type `(a: any) => void`, nor one made as
/// `(a: any) => …` in a place of type `(h: HttpRequest) => void`, because a function value
/// converted to another function type is a new value made as that type.
fn unify(cx: &Ctx, p: TyId, a: TyId, exact: bool, depth: u32) -> bool {
    // Deep enough: count it as a match (the join may only grow).
    if depth > 8 {
        return true;
    }
    let (p, a) = (strip(cx, p), strip(cx, a));
    if p == a || open_as(cx, p, exact) || open_as(cx, a, exact) {
        return true;
    }
    let (kp, ka) = (cx.ty.kind(p), cx.ty.kind(a));
    match (kp, ka) {
        (TyKind::Dyn(iface, _), TyKind::Dyn(other, _)) => iface == other,
        (TyKind::Dyn(iface, _), _) => cx.impls.iter().any(|i| i.iface == *iface && i.ty == a),
        (TyKind::Adt(pd, pa), TyKind::Adt(ad, aa)) if pd == ad => {
            pa.len() == aa.len()
                && pa
                    .iter()
                    .zip(aa)
                    .all(|(x, y)| unify(cx, *x, *y, exact, depth + 1))
        }
        (TyKind::Adt(pd, _), TyKind::Adt(ad, _)) => descends(cx, *ad, *pd),
        _ if std::mem::discriminant(kp) == std::mem::discriminant(ka) => {
            let (cp, ca) = (crate::types::children(kp), crate::types::children(ka));
            cp.len() == ca.len()
                && cp
                    .iter()
                    .zip(&ca)
                    .all(|(x, y)| unify(cx, *x, *y, exact, depth + 1))
        }
        _ => false,
    }
}

/// Is class `d` class `base` or one of its subclasses?
fn descends(cx: &Ctx, d: DefId, base: DefId) -> bool {
    let mut d = d;
    for _ in 0..64 {
        if d == base {
            return true;
        }
        let Some(TyKind::Adt(b, _)) = cx.adt(d).and_then(|a| a.base).map(|b| cx.ty.kind(b)) else {
            return false;
        };
        d = *b;
    }
    false
}
