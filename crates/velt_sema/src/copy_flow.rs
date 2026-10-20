//! Where the copies object conversions make can go ([`crate::object_copies`]): a conservative
//! flow of object values between locals, parameters and function results. A copy that reaches
//! anything else (a field, an array element, a closure called through a value, a call it cannot
//! follow) is said to reach the heap, and every value read from the heap may be it.
//!
//! The copy rule uses it for dynamic looks: converting a value to `any` shows a copy only when
//! that value may be the copy (or the copy reached the heap, where any value of a type holding
//! the converted-to type may hold it).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{
    Callee, Conversion, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, Pat, PatKind, Stmt,
    StmtKind as S, TyId,
};
use crate::visit::{self, VisitMut};

/// Where an object value may come from or go to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Store {
    /// A local or parameter of a function (`DefId(u32::MAX)`: a constant's initializer).
    Local(DefId, LocalId),
    /// What a function returns.
    Ret(DefId),
    /// Fields, elements and everything not followed.
    Heap,
    /// The value an object conversion built (`Copies::copies[i]`).
    Copy(u32),
}

/// The function a constant's initializer is evaluated in, for [`Store::Local`].
pub(crate) const GLOBAL: DefId = DefId(u32::MAX);

/// Which copy the converted object literal `e` builds, by its span and type.
fn copy_index(e: &Expr, copies: &[(Span, TyId)]) -> Option<u32> {
    copies
        .iter()
        .position(|&(s, t)| s == e.span && t == e.ty)
        .map(|i| i as u32)
}

/// Where the value of `e` (in function `f`) may come from.
pub(crate) fn sources(f: DefId, e: &Expr, copies: &[(Span, TyId)]) -> Vec<Store> {
    let mut out = vec![];
    collect_sources(f, e, copies, &mut out);
    out
}

fn collect_sources(f: DefId, e: &Expr, copies: &[(Span, TyId)], out: &mut Vec<Store>) {
    let add = |s: Store, out: &mut Vec<Store>| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    match &e.kind {
        E::AdtLit { converted, .. } if *converted != Conversion::No => {
            if let Some(i) = copy_index(e, copies) {
                add(Store::Copy(i), out);
            }
        }
        E::Local(l, _) => add(Store::Local(f, *l), out),
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => {
            if let Some(a) = args.first() {
                collect_sources(f, a, copies, out);
            }
        }
        E::Call {
            callee: Callee::Def(d, _),
            ..
        } => add(Store::Ret(*d), out),
        // A deep copy is a new object.
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Clone),
            ..
        } => {}
        E::Call { .. } | E::Field { .. } | E::Index { .. } | E::Global(_) => add(Store::Heap, out),
        E::If { then, els, .. } => {
            collect_sources(f, then, copies, out);
            collect_sources(f, els, copies, out);
        }
        E::Logical { lhs, rhs, .. } => {
            collect_sources(f, lhs, copies, out);
            collect_sources(f, rhs, copies, out);
        }
        E::Block(b) => {
            if let Some(v) = &b.value {
                collect_sources(f, v, copies, out);
            }
        }
        E::Match { arms, .. } => {
            for a in arms {
                collect_sources(f, &a.body, copies, out);
            }
        }
        E::Assign { value: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x) => collect_sources(f, x, copies, out),
        _ => {}
    }
}

/// The locals a pattern binds.
fn bindings(p: &Pat, out: &mut Vec<LocalId>) {
    match &p.kind {
        PatKind::Binding(l, _) => out.push(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|p| bindings(p, out))
        }
        PatKind::Adt { fields } => fields.iter().for_each(|(_, p)| bindings(p, out)),
        PatKind::Array { elems, rest } => {
            elems.iter().for_each(|p| bindings(p, out));
            out.extend(rest.iter().copied());
        }
        PatKind::Some(p) => bindings(p, out),
        _ => {}
    }
}

/// Intrinsics that neither keep their arguments nor give them back.
fn keeps_nothing(i: Intrinsic) -> bool {
    matches!(
        i,
        Intrinsic::Print
            | Intrinsic::PrintErr
            | Intrinsic::Same
            | Intrinsic::Eq
            | Intrinsic::KeyEq
            | Intrinsic::IncludesEq
            | Intrinsic::Hash
            | Intrinsic::Clone
            | Intrinsic::JsonStringify
            | Intrinsic::AnyStrictEq
            | Intrinsic::AnyLooseEq
            | Intrinsic::AnyTypeof
            | Intrinsic::AnyTruthy
    )
}

/// The flow edges of a program: each store's possible destinations.
struct Edges<'a> {
    f: DefId,
    copies: &'a [(Span, TyId)],
    /// The parameters (locals) of each function.
    params: &'a FxHashMap<DefId, Vec<LocalId>>,
    to: FxHashMap<Store, Vec<Store>>,
}

impl Edges<'_> {
    fn flow(&mut self, from: &Expr, to: Store) {
        for s in sources(self.f, from, self.copies) {
            let v = self.to.entry(s).or_default();
            if !v.contains(&to) {
                v.push(to);
            }
        }
    }

    fn link(&mut self, from: Store, to: Store) {
        self.to.entry(from).or_default().push(to);
    }

    /// `d` may be called where the flow cannot follow (through a value, virtually).
    fn called_anywhere(&mut self, d: DefId) {
        self.link(Store::Ret(d), Store::Heap);
        let params = self.params;
        for &p in params.get(&d).into_iter().flatten() {
            self.link(Store::Heap, Store::Local(d, p));
        }
    }

    fn bind(&mut self, p: &Pat, init: &Expr) {
        let mut ls = vec![];
        bindings(p, &mut ls);
        let whole = matches!(p.kind, PatKind::Binding(..));
        for l in ls {
            self.flow(init, Store::Local(self.f, l));
            if !whole {
                self.link(Store::Heap, Store::Local(self.f, l));
            }
        }
    }
}

impl VisitMut for Edges<'_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(e),
            } => self.flow(e, Store::Local(self.f, *local)),
            S::LetPat { pat, init } => self.bind(pat, init),
            S::Return(Some(e)) => self.flow(e, Store::Ret(self.f)),
            S::ForOf { binding, iter, .. } => {
                self.bind(binding, iter);
                let mut ls = vec![];
                bindings(binding, &mut ls);
                for l in ls {
                    self.link(Store::Heap, Store::Local(self.f, l));
                }
            }
            S::Try {
                catch: Some((Some(l), _)),
                ..
            } => self.link(Store::Heap, Store::Local(self.f, *l)),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Assign { place, value } => match place.kind {
                E::Local(l, _) => self.flow(value, Store::Local(self.f, l)),
                _ => self.flow(value, Store::Heap),
            },
            E::Call {
                callee: Callee::Def(d, _),
                args,
            } => match self.params.get(d) {
                Some(ps) if ps.len() == args.len() => {
                    let ps = ps.clone();
                    for (a, &p) in args.iter().zip(ps.iter()) {
                        self.flow(a, Store::Local(*d, p));
                    }
                }
                _ => args.iter().for_each(|a| self.flow(a, Store::Heap)),
            },
            E::Call {
                callee: Callee::Intrinsic(i),
                ..
            } if keeps_nothing(*i) || *i == Intrinsic::Share => {}
            E::Call { args, .. } => args.iter().for_each(|a| self.flow(a, Store::Heap)),
            E::AdtLit {
                fields: xs,
                converted: Conversion::No,
                ..
            }
            | E::Variant { args: xs, .. }
            | E::ArrayLit(xs)
            | E::Tuple(xs)
            | E::New { args: xs, .. } => xs.iter().for_each(|x| self.flow(x, Store::Heap)),
            E::Throw(x) => self.flow(x, Store::Heap),
            // A function used as a value is called through it: its parameters and its result
            // are the heap's.
            E::Closure(d) | E::FnRef(d, _) => self.called_anywhere(*d),
            E::Match { scrutinee, arms } => {
                for a in arms {
                    self.bind(&a.pat, scrutinee);
                }
            }
            _ => {}
        }
    }
}

/// Where each copy may go.
pub(crate) struct CopyFlow {
    /// The stores each copy reaches (`None`: the heap among them).
    reach: Vec<Option<FxHashSet<Store>>>,
}

impl CopyFlow {
    /// Build the flow for the copies at `copies` (span and converted-to type, in
    /// `Copies::copies` order).
    pub(crate) fn new(cx: &mut Ctx, copies: &[(Span, TyId)]) -> CopyFlow {
        let mut params: FxHashMap<DefId, Vec<LocalId>> = FxHashMap::default();
        let mut captures: Vec<(DefId, LocalId, LocalId)> = vec![];
        for (i, d) in cx.defs.iter().enumerate() {
            if let Some(Def::Fn(f)) = d {
                let d = DefId(i as u32);
                params.insert(d, f.params.iter().map(|p| p.local).collect());
                captures.extend(f.captures.iter().map(|c| (d, c.outer, c.inner)));
            }
        }
        let mut edges = Edges {
            f: GLOBAL,
            copies,
            params: &params,
            to: FxHashMap::default(),
        };
        for (i, d) in cx.defs.iter_mut().enumerate() {
            if let Some(Def::Fn(f)) = d {
                edges.f = DefId(i as u32);
                visit::block(&mut f.body.block, &mut edges);
                if let Some(v) = &f.body.block.value {
                    edges.flow(v, Store::Ret(edges.f));
                }
            }
        }
        // Methods may be called virtually or through an interface.
        for (i, d) in cx.defs.iter().enumerate() {
            if let Some(Def::Fn(f)) = d {
                if f.self_ty.is_some() {
                    edges.called_anywhere(DefId(i as u32));
                }
            }
        }
        edges.f = GLOBAL;
        for i in cx.info.iter_mut() {
            if let crate::defs::DefInfo::Global(g) = i {
                if let Some(e) = &mut g.init {
                    visit::expr(e, &mut edges);
                    edges.flow(e, Store::Heap);
                }
            }
        }
        // A closure's captures are its enclosing function's variables: both ways. The
        // enclosing function is found from the closure expression.
        let mut outer: FxHashMap<DefId, DefId> = FxHashMap::default();
        for (i, d) in cx.defs.iter_mut().enumerate() {
            if let Some(Def::Fn(f)) = d {
                visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
                    if let E::Closure(c) = e.kind {
                        outer.insert(c, DefId(i as u32));
                    }
                });
            }
        }
        for (c, o, inner) in captures {
            match outer.get(&c) {
                Some(&f) => {
                    edges.link(Store::Local(f, o), Store::Local(c, inner));
                    edges.link(Store::Local(c, inner), Store::Local(f, o));
                }
                None => edges.link(Store::Heap, Store::Local(c, inner)),
            }
        }
        let to = edges.to;
        let reach = (0..copies.len() as u32)
            .map(|i| {
                let mut seen: FxHashSet<Store> = FxHashSet::default();
                let mut work = vec![Store::Copy(i)];
                while let Some(s) = work.pop() {
                    if !seen.insert(s) {
                        continue;
                    }
                    if s == Store::Heap {
                        return None;
                    }
                    work.extend(to.get(&s).into_iter().flatten().copied());
                }
                Some(seen)
            })
            .collect();
        CopyFlow { reach }
    }

    /// No flow: every value may be any copy.
    pub(crate) fn none() -> CopyFlow {
        CopyFlow { reach: vec![] }
    }

    /// May a value from `srcs` be copy `c`?
    pub(crate) fn carries(&self, c: usize, srcs: &[Store]) -> bool {
        match self.reach.get(c) {
            Some(Some(r)) => srcs.iter().any(|s| r.contains(s)),
            _ => true,
        }
    }
}
