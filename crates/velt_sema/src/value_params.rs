//! Which params of each function may reach a function value (#2527): `run(q)` calling `KK(q)`, or
//! `r2(q)` that does, directly or through a closure it makes. Such a param reads as a borrow in
//! `run`'s signature, but what it holds can be replaced under whoever lent it: a `const x = a.c`
//! before `run(a)`, an element a loop is bound to, or an argument `run(a, a.c.s)` beside it. So can
//! a param a function modifies though it is `Borrow`, as it is used as a value too
//! (`finish::demote_fn_values`): a `const r = q` hides that from `q`'s caller.
//! A local bound or assigned a path from a param holds it too.
//! A method chosen at run time (virtual, interface, bounded type param) counts as every method of
//! its name, as its modes are fixed like a function value's: an override that calls `KK(q)` hides
//! it from `go`'s callers just the same.
//! `new C(q)` is a call of `C`'s constructor.
//! Asked by the exclusivity check (`const`s, loops) and by lowering (`stable_borrow`); one scan of
//! every body, then a walk up the callers from what changed.

use rustc_hash::FxHashMap;

use crate::replaced_holders::member;

use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, LocalId, PassMode, Pat, PatKind,
    Program, Stmt, StmtKind as S, TyKind,
};

/// For each function, the params (bit `i`: `params[i]`, the first 64) that may reach a function
/// value.
#[derive(Default)]
pub struct ValueParams {
    bits: FxHashMap<DefId, u64>,
    params: FxHashMap<DefId, usize>,
    /// Per method name, the params that may reach one in any method of that name, counted from
    /// the end (bit `k`: the `k`th from the last), as a closure's captures and `this` come first.
    methods: FxHashMap<String, u64>,
    /// The constructor `new` of each class called in a body runs.
    ctors: FxHashMap<DefId, DefId>,
}

impl ValueParams {
    /// Of a call of `def` with `args` arguments, the arguments (bit `i`: argument `i`) that may
    /// reach a function value. A closure's captures come before the params its arguments fill.
    pub fn of_args(&self, def: DefId, args: usize) -> u64 {
        let bits = self.bits.get(&def).copied().unwrap_or(0);
        let skip = (self.params.get(&def).copied().unwrap_or(0)).saturating_sub(args);
        match skip < 64 {
            true => bits >> skip,
            false => 0,
        }
    }

    /// Of `new` of `class` with `args` arguments, the arguments that may reach a function value.
    pub fn of_new(&self, class: DefId, args: usize) -> u64 {
        self.ctors
            .get(&class)
            .map_or(0, |ctor| self.of_args(*ctor, args))
    }

    /// Of a call with `args` arguments of the method named `name` (its full name or not), chosen
    /// at run time, the arguments that may reach a function value: all of them with no name.
    pub fn of_method(&self, name: Option<&str>, args: usize) -> u64 {
        let Some(name) = name else {
            return u64::MAX;
        };
        let from_end = self.methods.get(member(name)).copied().unwrap_or(0);
        (0..args.min(64))
            .filter(|i| {
                let k = args - 1 - i;
                k < 64 && from_end >> k & 1 == 1
            })
            .fold(0, |bits, i| bits | 1 << i)
    }
}

/// What a call needs of the program, which the checker and lowering hold differently.
pub trait Calls<'n> {
    /// The name of the method `callee` chooses at run time, when `callee` is such a call and the
    /// receiver's type says.
    fn method_name(&self, callee: &Callee, args: &[Expr]) -> Option<&'n str>;
    /// The constructor `new` of `class` runs (a subclass with none written has one that calls the
    /// base's).
    fn ctor(&self, class: DefId) -> Option<DefId>;
}

/// A param's reach passed on: to `param` of a callee, or argument `from_end` from its end.
struct Edge {
    caller: DefId,
    bits: u64,
    param: Param,
}

enum Param {
    At(usize),
    FromEnd(usize),
}

/// What one body does with its params.
struct Scan<'s, 'n> {
    def: DefId,
    calls: &'s dyn Calls<'n>,
    ctors: FxHashMap<DefId, DefId>,
    /// Each closure's captures, in the order of the params that hold them.
    captures: &'s FxHashMap<DefId, Vec<LocalId>>,
    /// Per local: the params its value comes from.
    roots: FxHashMap<LocalId, u64>,
    direct: u64,
    /// An assignment added to `roots`: scan again.
    grew: bool,
    edges: Vec<(DefId, Edge)>,
    /// Edges to every method of a name.
    methods: Vec<(&'n str, Edge)>,
}

/// [`compute`] of a finished program.
pub fn of_program(hir: &Program) -> ValueParams {
    let fns = hir.defs.iter().enumerate().filter_map(|(i, d)| match d {
        Def::Fn(f) => Some((DefId(i as u32), f)),
        _ => None,
    });
    compute(fns, &Lowered(hir))
}

/// [`Calls`] of a finished program.
struct Lowered<'h>(&'h Program);

impl<'h> Lowered<'h> {
    fn iface_method(&self, d: DefId, slot: u32) -> Option<&'h str> {
        match self.0.def(d) {
            Def::Interface(i) => i.methods.get(slot as usize).map(|m| m.name.as_str()),
            _ => None,
        }
    }
}

impl<'h> Calls<'h> for Lowered<'h> {
    fn method_name(&self, callee: &Callee, args: &[Expr]) -> Option<&'h str> {
        let hir = self.0;
        match callee {
            Callee::Virtual { slot } => {
                let TyKind::Adt(class, _) = hir.types.kind(args.first()?.ty) else {
                    return None;
                };
                let Def::Adt(a) = hir.def(*class) else {
                    return None;
                };
                match hir.def(*a.vtable.get(*slot as usize)?) {
                    Def::Fn(f) => Some(f.name.as_str()),
                    _ => None,
                }
            }
            Callee::Dyn { slot, .. } => match hir.types.kind(args.first()?.ty) {
                TyKind::Dyn(iface, _) => self.iface_method(*iface, *slot),
                _ => None,
            },
            Callee::ParamMethod { iface, slot, .. } => self.iface_method(*iface, *slot),
            _ => None,
        }
    }

    fn ctor(&self, class: DefId) -> Option<DefId> {
        match self.0.def(class) {
            Def::Adt(a) => a.ctor,
            _ => None,
        }
    }
}

/// Every function's params that may reach a function value.
pub fn compute<'a>(
    fns: impl Iterator<Item = (DefId, &'a FnDef)>,
    calls: &dyn Calls<'a>,
) -> ValueParams {
    let mut out: FxHashMap<DefId, u64> = FxHashMap::default();
    let mut params: FxHashMap<DefId, usize> = FxHashMap::default();
    let mut names: FxHashMap<DefId, &str> = FxHashMap::default();
    let mut callers: FxHashMap<DefId, Vec<Edge>> = FxHashMap::default();
    let mut method_callers: FxHashMap<&str, Vec<Edge>> = FxHashMap::default();
    let mut ctors: FxHashMap<DefId, DefId> = FxHashMap::default();
    let mut work = vec![];
    let fns: Vec<(DefId, &FnDef)> = fns.collect();
    let captures: FxHashMap<DefId, Vec<LocalId>> = (fns.iter())
        .filter(|(_, f)| !f.captures.is_empty())
        .map(|(d, f)| (*d, f.captures.iter().map(|c| c.outer).collect()))
        .collect();
    for (def, f) in fns {
        params.insert(def, f.params.len());
        names.insert(def, member(&f.name));
        let mut s = Scan {
            def,
            calls,
            ctors: FxHashMap::default(),
            captures: &captures,
            roots: FxHashMap::default(),
            direct: 0,
            grew: false,
            edges: vec![],
            methods: vec![],
        };
        for (i, p) in f.params.iter().enumerate().take(64) {
            s.roots.insert(p.local, 1 << i);
            if p.mode == PassMode::Borrow && f.body.locals[p.local.0 as usize].mutable {
                s.direct |= 1 << i;
            }
        }
        let direct = s.direct;
        s.block(&f.body.block);
        // An assignment reaches uses before it in a loop, and ones scanned before it was.
        while std::mem::take(&mut s.grew) {
            s.direct = direct;
            s.edges.clear();
            s.methods.clear();
            s.block(&f.body.block);
        }
        ctors.extend(s.ctors.drain());
        for (callee, e) in s.edges {
            callers.entry(callee).or_default().push(e);
        }
        for (name, e) in s.methods {
            method_callers.entry(member(name)).or_default().push(e);
        }
        if s.direct != 0 {
            out.insert(def, s.direct);
            work.push(def);
        }
    }
    while let Some(d) = work.pop() {
        let bits = out.get(&d).copied().unwrap_or(0);
        let count = params.get(&d).copied().unwrap_or(0);
        let by_def = callers.get(&d).into_iter().flatten();
        let by_name = names.get(&d).and_then(|n| method_callers.get(n));
        for e in by_def.chain(by_name.into_iter().flatten()) {
            let i = match e.param {
                Param::At(i) => i,
                Param::FromEnd(k) => match count.checked_sub(k + 1) {
                    Some(i) => i,
                    None => continue,
                },
            };
            if i >= 64 || bits >> i & 1 == 0 {
                continue;
            }
            let was = out.entry(e.caller).or_insert(0);
            if *was | e.bits != *was {
                *was |= e.bits;
                work.push(e.caller);
            }
        }
    }
    let mut methods: FxHashMap<String, u64> = FxHashMap::default();
    for (d, bits) in &out {
        let (Some(name), Some(&count)) = (names.get(d), params.get(d)) else {
            continue;
        };
        for i in (0..count.min(64)).filter(|i| bits >> i & 1 == 1) {
            let k = count - 1 - i;
            if k < 64 {
                *methods.entry(name.to_string()).or_insert(0) |= 1 << k;
            }
        }
    }
    ValueParams {
        bits: out,
        params,
        methods,
        ctors,
    }
}

/// The local the place `e` starts from, through shares and conversions.
fn root(e: &Expr) -> Option<LocalId> {
    let mut x = e;
    loop {
        match &x.kind {
            E::Local(l, _) => return Some(*l),
            E::Field { base: b, .. }
            | E::Index { base: b, .. }
            | E::UnwrapSome(b, _)
            | E::UnwrapVariant { expr: b, .. }
            | E::Upcast(b)
            | E::Downcast(b)
            | E::WrapSome(b)
            | E::Cast(b)
            | E::ToDyn { expr: b, .. } => x = b,
            E::Call {
                callee: Callee::Intrinsic(crate::hir::Intrinsic::Share),
                args,
            } if args.len() == 1 => x = &args[0],
            _ => return None,
        }
    }
}

impl Scan<'_, '_> {
    fn bits_of(&self, e: &Expr) -> u64 {
        root(e).map_or(0, |l| self.roots.get(&l).copied().unwrap_or(0))
    }

    fn block(&mut self, b: &Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
        if let Some(e) = &b.value {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            S::Let { local, init } => {
                if let Some(e) = init {
                    let bits = self.bits_of(e);
                    if bits != 0 {
                        *self.roots.entry(*local).or_insert(0) |= bits;
                    }
                    self.expr(e);
                }
            }
            S::LetPat { pat, init } => {
                let bits = self.bits_of(init);
                if bits != 0 {
                    let mut ls = vec![];
                    pat_locals(pat, &mut ls);
                    for l in ls {
                        *self.roots.entry(l).or_insert(0) |= bits;
                    }
                }
                self.expr(init);
            }
            S::Expr(e) | S::Return(Some(e)) => self.expr(e),
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|b| self.block(b));
            }
            S::While {
                cond, body, step, ..
            } => {
                self.expr(cond);
                self.block(body);
                step.iter().for_each(|e| self.expr(e));
            }
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                let bits = self.bits_of(iter);
                if bits != 0 {
                    let mut ls = vec![];
                    pat_locals(binding, &mut ls);
                    for l in ls {
                        *self.roots.entry(l).or_insert(0) |= bits;
                    }
                }
                self.expr(iter);
                self.block(body);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                catch.iter().for_each(|(_, b)| self.block(b));
                finally.iter().for_each(|b| self.block(b));
            }
            S::Block(b) => self.block(b),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Call { callee, args } => match callee {
                Callee::Indirect(_) => {
                    for a in args {
                        self.direct |= self.bits_of(a);
                    }
                }
                Callee::Def(d, _) => {
                    for (i, a) in args.iter().enumerate() {
                        let bits = self.bits_of(a);
                        if bits != 0 {
                            let param = Param::FromEnd(args.len() - 1 - i);
                            let caller = self.def;
                            let edge = Edge {
                                caller,
                                bits,
                                param,
                            };
                            self.edges.push((*d, edge));
                        }
                    }
                }
                Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                    let name = self.calls.method_name(callee, args);
                    for (i, a) in args.iter().enumerate() {
                        let bits = self.bits_of(a);
                        match name {
                            _ if bits == 0 => {}
                            Some(n) => {
                                let param = Param::FromEnd(args.len() - 1 - i);
                                let caller = self.def;
                                let edge = Edge {
                                    caller,
                                    bits,
                                    param,
                                };
                                self.methods.push((n, edge));
                            }
                            None => self.direct |= bits,
                        }
                    }
                }
                _ => {}
            },
            E::Assign { place, value } => {
                let bits = self.bits_of(value);
                if let Some(l) = root(place).filter(|_| bits != 0) {
                    let was = self.roots.entry(l).or_insert(0);
                    self.grew |= *was | bits != *was;
                    *was |= bits;
                }
            }
            E::New { def, args, .. } => {
                if let Some(ctor) = self.calls.ctor(*def) {
                    self.ctors.insert(*def, ctor);
                    for (i, a) in args.iter().enumerate() {
                        let bits = self.bits_of(a);
                        if bits != 0 {
                            let param = Param::FromEnd(args.len() - 1 - i);
                            let caller = self.def;
                            let edge = Edge {
                                caller,
                                bits,
                                param,
                            };
                            self.edges.push((ctor, edge));
                        }
                    }
                }
            }
            E::Closure(d) => {
                let outers = self.captures.get(d).cloned().unwrap_or_default();
                for (k, outer) in outers.into_iter().enumerate() {
                    let bits = self.roots.get(&outer).copied().unwrap_or(0);
                    if bits != 0 {
                        let caller = self.def;
                        let param = Param::At(k);
                        let edge = Edge {
                            caller,
                            bits,
                            param,
                        };
                        self.edges.push((*d, edge));
                    }
                }
            }
            _ => {}
        }
        if let E::Block(b) = &e.kind {
            return self.block(b);
        }
        children(e, &mut |x| self.expr(x));
    }
}

/// The expressions directly inside `e`, but for a block's.
fn children(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    match &e.kind {
        E::Lit(_)
        | E::ConstArray(_)
        | E::Global(_)
        | E::FnRef(..)
        | E::Closure(_)
        | E::Local(..)
        | E::Block(_) => {}
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::ToDyn { expr: x, .. }
        | E::Throw(x)
        | E::Field { base: x, .. } => f(x),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            f(place);
            f(value);
        }
        E::Index { base, index, .. } => {
            f(base);
            f(index);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                f(c);
            }
            args.iter().for_each(&mut *f);
        }
        E::If { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(&mut *f),
        E::Match { scrutinee, arms } => {
            f(scrutinee);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
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
