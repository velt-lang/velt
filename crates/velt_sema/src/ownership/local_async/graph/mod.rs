//! The value-flow graph of [`super`]: which closure literals, locals and return values a value
//! may come from, where it goes, and which values reach a thread boundary.
//!
//! Nodes are closure literals, locals (parameters and captures included) and the return value
//! of each function. An edge `n ← m` says that values of `m` may flow into `n`: an initializer
//! or assignment, an argument into the parameter of a direct call, a returned value into the
//! function's return, a call's result out of it, and a captured variable into the closure's
//! capture local. Values whose origin the graph does not follow (a field or element read, the
//! result of a call through a function value, a pattern binding) are *unknown*: when one
//! crosses, its type is a root of the type-based part (`super::types`).
//!
//! A call through a function value, an interface method or an overridden method is not a
//! boundary (#2726): the call runs on this task, and a callee that keeps an argument keeps it
//! in the heap, so the argument is *stored* like a value written into a field, and the heap rule
//! decides whether it crosses. A callee that runs a kept callback on another thread does so
//! through one of the boundaries here, whose run-time transfer copies what is still shared.

use rustc_hash::FxHashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo};
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Stmt, StmtKind as S,
    TyId,
};

mod bind;
mod dynamic;

use super::call_sites::{Held, Indirect};
pub(super) use super::why::{Boundary, Why};
pub(super) use dynamic::untraced_input;
pub(super) use dynamic::DynConv;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Node {
    /// The closure literal `def` (its value where it is created).
    Lit(DefId),
    Local(DefId, LocalId),
    /// What function `def` returns.
    Ret(DefId),
    /// Argument `1` of the call through a function value numbered `0` (`Graph::indirect`).
    Arg(u32, u32),
    /// Argument `1` of the dispatched call numbered `0` (`Graph::dispatched_calls`).
    DArg(u32, u32),
    /// The function value called by the call through a function value numbered `0`.
    Callee(u32),
    /// The value made a dynamic one (`any`, `unknown`, `object`, `Function`) by the conversion
    /// numbered `0` (`Graph::dynamic`): it passes through to where the converted value goes.
    Dyn(u32),
    /// The value matched by the `match` numbered `0` (`Graph::scrutinees`), when it isn't a
    /// local: its bindings take its values (#2893).
    Scrut(u32),
}

/// A dispatched call: the methods it may reach, the argument node at each position after the
/// receiver, and the call's span and whether it is in a library.
pub(super) type Dispatched = (Vec<DefId>, Vec<Option<usize>>, Span, bool);

/// Node flags.
pub(super) const CROSSES: u8 = 1;
pub(super) const STORED: u8 = 4;

/// The call passing an argument into a parameter: its span, and whether it is in a library.
pub(super) type Site = Option<(Span, bool)>;

#[derive(Default)]
pub(super) struct Graph {
    pub ids: FxHashMap<Node, usize>,
    pub nodes: Vec<Node>,
    pub tys: Vec<TyId>,
    /// Per node: the nodes whose values flow into it, each with the call passing them (an
    /// argument flowing into a parameter: the call's span and whether it is in a library).
    pub srcs: Vec<Vec<(usize, Site)>>,
    /// Per node: may it hold a value of unknown origin?
    pub unknown: Vec<bool>,
    /// Per node: the types of the values of unknown origin flowing into it, as written where
    /// they flow in (a generic parameter's node gets its callers' concrete types).
    pub inflows: Vec<Vec<(TyId, Site)>>,
    /// Calls through a function value: the callee's type, the argument nodes, and the call
    /// (span, in a library). A closure whose parameter crosses makes the matching argument cross.
    pub indirect: Vec<Indirect>,
    /// The type arguments of every direct call of each generic function, with the function
    /// making the call (its own generic parameters may appear in them).
    pub insts: FxHashMap<DefId, Vec<(DefId, Vec<TyId>)>>,
    /// Calls of generic function values, for their closures' instantiations.
    pub dyn_calls: super::generic_closures::DynCalls,
    /// The function creating each closure literal.
    pub parent: FxHashMap<DefId, DefId>,
    /// Where values go that the edges don't show (`super::call_sites::held_in_locals`).
    pub held: Held,
    /// Nodes whose values reach a flag's sink directly.
    pub seeds: Vec<(usize, u8, Option<Why>)>,
    /// Values of unknown origin that cross.
    pub roots: Vec<(TyId, Why)>,
    /// Named functions used as values (called through function values too), each with its type
    /// where it is used (the instantiation of a generic one).
    pub fn_values: Vec<(DefId, TyId)>,
    /// Virtual, interface and bounded method calls: the methods each may reach, its argument nodes
    /// by position (none for the receiver), and the call (span, in a library). A method's
    /// parameter that crosses makes the matching argument cross ([`super::crossing_params`]).
    pub dispatched_calls: Vec<Dispatched>,
    /// Values of unknown origin, outside std, of a type that mentions a type parameter, that cross
    /// where they are read: the function, the type and why (resolved like `wild` locals).
    pub generic_roots: Vec<(DefId, TyId, Why)>,
    /// The type arguments of every `new` or literal of each generic class or struct, with the
    /// function making it.
    pub news: FxHashMap<DefId, Vec<(DefId, Vec<TyId>)>>,
    /// The methods a virtual, interface or bounded call may reach (sorted): what such a method's
    /// type parameters are isn't known from its direct calls alone.
    pub dispatched: Vec<DefId>,
    /// Locals and results outside std whose type mentions a type parameter (in node order, so sorted). One that
    /// crosses carries what its type has in each instantiation of its function
    /// (`super::instances`), or any closure where no instantiation is known.
    pub wild: Vec<usize>,
    /// The `wild` nodes a value from std flows into directly (in node order, so sorted): resolved
    /// like values of unknown origin.
    pub std_fed: Vec<usize>,
    /// Values of the program's generic types flowing straight into a std node (`Promise.resolve(gs[0])`),
    /// by that node: the program function and the type, resolved through its instantiations when
    /// the node crosses.
    pub std_inflows: FxHashMap<usize, Vec<(DefId, TyId)>>,
    /// Per closure literal node: its capture locals' nodes.
    pub captures: FxHashMap<usize, Vec<usize>>,
    /// The conversions into a dynamic type ([`Node::Dyn`]): every value the program makes a dynamic
    /// one. A crossing value of a dynamic type (`keepAndSpawn<unknown>(job)`) may hold any of them
    /// (#2865), and nothing else.
    pub dynamic: Vec<DynConv>,
    /// How many [`Node::Scrut`] nodes there are.
    pub scrutinees: u32,
    /// A [`Node::Dyn`] whose input is a call through a function value, with that call's
    /// [`Node::Callee`]: the dynamic result comes from the function, which crosses with it
    /// (`() => Job` called through a `() => unknown`).
    pub dyn_callee: FxHashMap<usize, usize>,
    /// Nodes that a call through a function value's result flows into, with that call's
    /// [`Node::Callee`] (`const v = await f()`), for [`Graph::dyn_callee`].
    pub call_results: FxHashMap<usize, usize>,
    /// Per node: does something flow into it that the graph can't trace
    /// ([`dynamic::untraced_nodes`])?
    pub untraced: Vec<bool>,
    /// The calls through function values (`Graph::indirect`), by the type called through, in the
    /// order the types first appear.
    pub indirect_by_ty: Vec<(TyId, Vec<usize>)>,
}

impl Graph {
    fn node(&mut self, n: Node, ty: TyId) -> usize {
        if let Some(&i) = self.ids.get(&n) {
            return i;
        }
        let i = self.nodes.len();
        self.ids.insert(n, i);
        self.nodes.push(n);
        self.tys.push(ty);
        self.srcs.push(vec![]);
        self.unknown.push(false);
        self.inflows.push(vec![]);
        i
    }
}

/// Where a value goes.
#[derive(Clone, Copy)]
enum To {
    /// Used here (read, compared, called, borrowed, matched): nowhere the graph follows.
    Drop,
    /// Into a node, passed by the call at the span (an argument), if any.
    Node(usize, Option<(Span, bool)>),
    /// Into a sink: a boundary (`CROSSES`) or the heap (`STORED`), where an unknown callee
    /// (a call through a function value or an interface) may keep it too.
    Flag(u8, Option<Why>),
}

const STORE: To = To::Flag(STORED, None);

/// The graph of every checked function body.
pub(super) fn build(cx: &Ctx) -> Graph {
    let mut g = Graph::default();
    for i in 0..cx.defs.len() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = &cx.defs[i] else {
            continue;
        };
        let Some(info) = cx.try_fn(d) else {
            continue;
        };
        if info.state != BodyState::Done {
            continue;
        }
        let library = cx.scopes[info.module].is_library;
        let std = cx.scopes[info.module].is_std;
        let mut w = Walk {
            cx,
            g: &mut g,
            d,
            f,
            library,
            std,
        };
        let ret = w.ret_node(d);
        // What an async function returns is kept in its promise, in the heap, where a value of
        // unknown origin may be read back (`await g()` through a `() => Promise<unknown>`).
        if f.is_async {
            w.g.seeds.push((ret, STORED, None));
        }
        w.block(&f.body.block, To::Node(ret, None));
    }
    super::unknown_callees::mark(cx, &mut g);
    super::call_sites::named_outside_bodies(cx, &mut g.held.referenced);
    dynamic::finish(cx, &mut g);
    g.dyn_calls.finish(cx);
    g
}

struct Walk<'a, 'm> {
    cx: &'a Ctx<'m>,
    g: &'a mut Graph,
    d: DefId,
    f: &'a FnDef,
    library: bool,
    /// Is this function in std (`Graph::generic_roots` leaves std out)?
    std: bool,
}

impl Walk<'_, '_> {
    fn local(&mut self, l: LocalId) -> usize {
        let ty = self.f.body.locals[l.0 as usize].ty;
        self.g.node(Node::Local(self.d, l), ty)
    }

    fn ret_node(&mut self, d: DefId) -> usize {
        let ty = match &self.cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => f.ret,
            _ => self.cx.ty.unit,
        };
        self.g.node(Node::Ret(d), ty)
    }

    fn why(&self, span: Span, boundary: Boundary) -> Option<Why> {
        Some(Why {
            span,
            boundary,
            library: self.library,
            in_library: false,
            via: None,
            depth: 0,
            in_value: false,
            made_dyn: None,
        })
    }

    /// Values of node `n` go to `to`.
    fn connect(&mut self, n: usize, to: To) {
        match to {
            To::Drop => self.g.held.escapes.push(n),
            To::Node(m, site) => self.g.srcs[m].push((n, site)),
            To::Flag(f, why) => self.g.seeds.push((n, f, why)),
        }
    }

    /// The value of `e` has an origin the graph does not follow.
    fn unknown(&mut self, e: &Expr, to: To) {
        match to {
            To::Node(m, site) => {
                self.g.unknown[m] = true;
                if !self.g.inflows[m].contains(&(e.ty, site)) {
                    self.g.inflows[m].push((e.ty, site));
                }
                // A value of the program's generic type flowing straight into std: std's nodes
                // don't resolve the program's type parameters (#2768).
                let into_std = super::unknown_callees::std_node(self.cx, self.g, m);
                if into_std && !self.std && super::types::mentions_param(self.cx, e.ty) {
                    let entry = self.g.std_inflows.entry(m).or_default();
                    if !entry.contains(&(self.d, e.ty)) {
                        entry.push((self.d, e.ty));
                    }
                }
            }
            To::Flag(CROSSES, Some(why)) => {
                self.g.roots.push((e.ty, why));
                // A value of a generic type that crosses where it is made, with no local in
                // between (`spawn(gs[0]())`, a field, a call's result), is resolved like a local of
                // that type.
                if !self.std && super::types::mentions_param(self.cx, e.ty) {
                    self.g.generic_roots.push((self.d, e.ty, why));
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, b: &Block, to: To) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(v) = &b.value {
            self.value(v, to);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            S::Let { local, init } => {
                if let Some(init) = init {
                    let n = self.local(*local);
                    self.value(init, To::Node(n, None));
                }
            }
            S::LetPat { pat, init } => {
                self.value(init, To::Drop);
                self.bind(pat);
            }
            S::Expr(e) => self.value(e, To::Drop),
            S::Return(e) => {
                if let Some(e) = e {
                    let n = self.ret_node(self.d);
                    self.value(e, To::Node(n, None));
                }
            }
            S::If { cond, then, els } => {
                self.value(cond, To::Drop);
                self.block(then, To::Drop);
                if let Some(b) = els {
                    self.block(b, To::Drop);
                }
            }
            S::While {
                cond, body, step, ..
            } => {
                self.value(cond, To::Drop);
                self.block(body, To::Drop);
                if let Some(s) = step {
                    self.value(s, To::Drop);
                }
            }
            S::ForOf {
                binding,
                iter,
                body,
                ..
            } => {
                self.value(iter, To::Drop);
                self.bind(binding);
                self.block(body, To::Drop);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body, To::Drop);
                if let Some((l, b)) = catch {
                    if let Some(l) = l {
                        let n = self.local(*l);
                        self.g.unknown[n] = true;
                    }
                    self.block(b, To::Drop);
                }
                if let Some(b) = finally {
                    self.block(b, To::Drop);
                }
            }
            S::Break(_) | S::Continue(_) => {}
            S::Block(b) => self.block(b, To::Drop),
        }
    }

    /// Pattern bindings hold parts of a value: an origin the graph does not follow.
    fn value(&mut self, e: &Expr, to: To) {
        match &e.kind {
            E::Local(l, _) => {
                let n = self.local(*l);
                self.connect(n, to);
            }
            E::Closure(c) => self.closure(*c, e.ty, to),
            E::If { cond, then, els } => {
                self.value(cond, To::Drop);
                self.value(then, to);
                self.value(els, to);
            }
            E::Block(b) => self.block(b, to),
            E::Match { scrutinee, arms } => {
                let from = Some(self.matched(scrutinee));
                for a in arms {
                    self.bind_from(&a.pat, from);
                    if let Some(g) = &a.guard {
                        self.value(g, To::Drop);
                    }
                    self.value(&a.body, to);
                }
            }
            // A generic function value converted to another type may be called where the walk
            // doesn't see it (`super::generic_closures`).
            E::Cast(x) | E::Upcast(x) | E::Downcast(x) => {
                self.g.dyn_calls.escape(self.cx, x.ty);
                self.value(x, to)
            }
            // A value made a dynamic one (`any`, `unknown`, `object`): a dynamic call may reach
            // what it holds (`super::generic_closures`). A generic function value's own `ToDyn`
            // (its interface) is no conversion.
            E::ToDyn { expr: x, .. } => {
                if !super::types::mentions_dynamic(self.cx, e.ty) {
                    return self.value(x, to);
                }
                self.g.dyn_calls.escape(self.cx, x.ty);
                // Only into a dynamic type itself: an interface whose type arguments mention one
                // (`AsyncIterator<T, unknown>`) doesn't make the value a dynamic one. Nor does a
                // function's conversion, unless it makes what the function returns dynamic
                // (`() => Job` as `() => unknown`): then the function value is what is made dynamic.
                let returns = super::types::returns_newly_dynamic(self.cx, x.ty, e.ty);
                if !returns && !super::types::is_dynamic(self.cx, e.ty) {
                    return self.value(x, to);
                }
                self.made_dynamic(x, e.span, Some(e.ty), to);
            }
            E::WrapSome(x)
            | E::UnwrapSome(x, _)
            | E::Await(x)
            | E::UnwrapVariant { expr: x, .. } => self.value(x, to),
            E::Logical { lhs, rhs, .. } => {
                self.value(lhs, to);
                self.value(rhs, to);
            }
            E::Call { callee, args } => self.call(e, callee, args, to),
            E::New {
                def,
                type_args,
                args,
            } => {
                // Every object of a generic class is made here: its instantiations.
                if !type_args.is_empty() {
                    let site = (self.d, type_args.clone());
                    self.g.news.entry(*def).or_default().push(site);
                }
                let ctor = match &self.cx.info[def.0 as usize] {
                    DefInfo::Adt(a) => a.ctor,
                    _ => None,
                };
                self.pass_to_params(ctor, args, e.span);
                self.unknown(e, to);
            }
            E::AdtLit {
                def,
                type_args,
                fields: xs,
                ..
            } => {
                if !type_args.is_empty() {
                    let site = (self.d, type_args.clone());
                    self.g.news.entry(*def).or_default().push(site);
                }
                for x in xs {
                    self.value(x, STORE);
                }
                self.unknown(e, to);
            }
            E::ArrayLit(xs) | E::Tuple(xs) | E::Variant { args: xs, .. } => {
                for x in xs {
                    self.value(x, STORE);
                }
                self.unknown(e, to);
            }
            E::Assign { place, value } => {
                match &place.kind {
                    E::Local(l, _) => {
                        let n = self.local(*l);
                        self.value(value, To::Node(n, None));
                    }
                    _ => {
                        self.value(place, To::Drop);
                        self.value(value, STORE);
                    }
                }
                self.unknown(e, to);
            }
            E::CompoundAssign { place, value, .. } => {
                self.value(place, To::Drop);
                self.value(value, To::Drop);
                self.unknown(e, to);
            }
            E::Field { base, .. } => {
                self.value(base, To::Drop);
                self.unknown(e, to);
            }
            E::Index { base, index, .. } => {
                self.value(base, To::Drop);
                self.value(index, To::Drop);
                self.unknown(e, to);
            }
            E::Unary { expr, .. } => {
                self.value(expr, To::Drop);
                self.unknown(e, to);
            }
            E::Binary { lhs, rhs, .. } => {
                self.value(lhs, To::Drop);
                self.value(rhs, To::Drop);
                self.unknown(e, to);
            }
            // A thrown value may be caught as an `unknown` anywhere up the calls (`catch (e)`, a
            // rejected promise's `catch`): made a dynamic value (#2865).
            E::Throw(x) => self.made_dynamic(x, e.span, None, STORE),
            // A `ConstArray`'s elements are literals, which flow nowhere (as an `ArrayLit`'s).
            E::FnRef(d, targs) => {
                self.g.fn_values.push((*d, e.ty));
                self.g.held.referenced.insert(*d);
                // A generic function used as a value is one more instantiation of it.
                if !targs.is_empty() {
                    let site = (self.d, targs.clone());
                    self.g.insts.entry(*d).or_default().push(site);
                }
                self.unknown(e, to)
            }
            E::Lit(_) | E::ConstArray(_) | E::Global(_) => self.unknown(e, to),
        }
    }

    /// A closure literal: its captured variables flow into its capture locals.
    fn closure(&mut self, c: DefId, ty: TyId, to: To) {
        self.g.parent.insert(c, self.d);
        let n = self.g.node(Node::Lit(c), ty);
        if !self.g.captures.contains_key(&n) {
            let mut inner = vec![];
            if let Some(Def::Fn(cf)) = &self.cx.defs[c.0 as usize] {
                for cap in &cf.captures {
                    let cty = cf.body.locals[cap.inner.0 as usize].ty;
                    let i = self.g.node(Node::Local(c, cap.inner), cty);
                    let o = self.local(cap.outer);
                    self.g.srcs[i].push((o, None));
                    // And back: what the closure assigns to the captured variable is the
                    // outer variable's value too (`set = () => { u = job; }`, #2893).
                    self.g.srcs[o].push((i, None));
                    inner.push(i);
                }
            }
            self.g.captures.insert(n, inner);
        }
        self.connect(n, to);
    }

    /// The arguments of a direct call of `callee` (a function, or a constructor whose first
    /// parameter is `this`) flow into its parameters; anything else they are passed to is
    /// treated as storing them.
    fn pass_to_params(&mut self, callee: Option<DefId>, args: &[Expr], span: Span) {
        let params: Option<Vec<LocalId>> = callee.and_then(|g| match &self.cx.defs[g.0 as usize] {
            Some(Def::Fn(gf)) if gf.params.len() >= args.len() => {
                let skip = gf.params.len() - args.len();
                Some(gf.params[skip..].iter().map(|p| p.local).collect())
            }
            _ => None,
        });
        let (Some(g), Some(params)) = (callee, params) else {
            for a in args {
                self.value(a, STORE);
            }
            return;
        };
        let Some(Def::Fn(gf)) = &self.cx.defs[g.0 as usize] else {
            return;
        };
        for (a, l) in args.iter().zip(params) {
            let ty = gf.body.locals[l.0 as usize].ty;
            let n = self.g.node(Node::Local(g, l), ty);
            self.value(a, To::Node(n, Some((span, self.library))));
        }
    }

    /// Is `f` a read of a variable holding an accessor that `Object.defineProperty` replaces
    /// (`crate::defined_accessors::is_accessor_cell`)?
    fn accessor_cell(&self, mut f: &Expr) -> bool {
        loop {
            match &f.kind {
                E::UnwrapSome(x, _) | E::Cast(x) | E::UnwrapVariant { expr: x, .. } => f = x,
                E::Local(l, _) => {
                    let name = &self.f.body.locals[l.0 as usize].name;
                    return crate::defined_accessors::is_accessor_cell(name);
                }
                _ => return false,
            }
        }
    }

    fn call(&mut self, e: &Expr, callee: &Callee, args: &[Expr], to: To) {
        match callee {
            Callee::Def(g, targs) => {
                if !targs.is_empty() {
                    let site = (self.d, targs.clone());
                    self.g.insts.entry(*g).or_default().push(site);
                }
                let direct = matches!(&self.cx.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.params.len() >= args.len());
                self.pass_to_params(Some(*g), args, e.span);
                *self.g.held.direct.entry(*g).or_insert(true) &= direct;
                if direct {
                    let n = self.ret_node(*g);
                    self.connect(n, to);
                } else {
                    self.unknown(e, to);
                }
            }
            Callee::Indirect(f) => {
                let call = self.g.indirect.len() as u32;
                // Where the called value comes from, so that a closure that stays in locals is
                // matched only with the calls it may reach (`super::call_sites`).
                let callee = self.g.node(Node::Callee(call), f.ty);
                self.value(f, To::Node(callee, None));
                // A variable of `Object.defineProperty`'s rewrite holds only closures of this
                // function, which keep nothing as a function value might.
                let cell = self.accessor_cell(f);
                let mut nodes = vec![];
                for (i, a) in args.iter().enumerate() {
                    let n = self.g.node(Node::Arg(call, i as u32), a.ty);
                    self.value(a, To::Node(n, None));
                    // The callee runs on this task; what it keeps, it keeps in the heap.
                    if !cell {
                        self.g.seeds.push((n, STORED, None));
                    }
                    nodes.push(n);
                }
                self.g.indirect.push(Indirect {
                    ty: f.ty,
                    callee,
                    args: nodes,
                    span: e.span,
                    library: self.library,
                    only: cell.then_some(self.d),
                });
                if let To::Node(m, _) = to {
                    self.g.call_results.insert(m, callee);
                }
                self.unknown(e, to);
            }
            Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                // What the callee keeps, it keeps in the heap; a parameter of a method it may
                // reach that crosses makes the argument cross (`super::crossing_params`).
                let call = self.g.dispatched_calls.len() as u32;
                let mut nodes = vec![];
                for (i, a) in args.iter().enumerate() {
                    if i == 0 {
                        self.value(a, To::Drop);
                        nodes.push(None);
                        continue;
                    }
                    let n = self.g.node(Node::DArg(call, i as u32), a.ty);
                    self.value(a, To::Node(n, None));
                    self.g.seeds.push((n, STORED, None));
                    nodes.push(Some(n));
                }
                if let (
                    Callee::Dyn {
                        slot,
                        method_type_args,
                    },
                    Some(r),
                ) = (callee, args.first())
                {
                    (self.g.dyn_calls).call(self.cx, self.d, r.ty, *slot, method_type_args);
                }
                if let Callee::ParamMethod {
                    iface,
                    slot,
                    method_type_args,
                    ..
                } = callee
                {
                    (self.g.dyn_calls).bounded(*iface, self.d, *slot, method_type_args);
                }
                let targets = super::unknown_callees::dispatch_targets(self.cx, callee, args);
                self.g
                    .dispatched_calls
                    .push((targets, nodes, e.span, self.library));
                self.unknown(e, to);
            }
            Callee::Intrinsic(i) => self.intrinsic(e, *i, args, to),
        }
    }

    fn intrinsic(&mut self, e: &Expr, i: Intrinsic, args: &[Expr], to: To) {
        // A generic function value going into `any` (or copied by a spread) may be called where
        // the walk doesn't see it (`super::generic_closures`).
        if matches!(i, Intrinsic::ToAny | Intrinsic::SpreadObject) {
            for a in args {
                self.g.dyn_calls.escape(self.cx, a.ty);
            }
        }
        // What only inspects a borrowed `any` (`typeof x`, `x === y`, `Array.isArray(x)`) keeps
        // nothing: a value converted to `any` for it is read here, not made a dynamic value.
        if dynamic::inspects_only(i) {
            for a in args {
                match &a.kind {
                    E::ToDyn { expr: x, .. } => self.value(x, To::Drop),
                    _ => self.value(a, To::Drop),
                }
            }
            return self.unknown(e, to);
        }
        // A share of the value as an `any` (kept, as other intrinsics' arguments are).
        if let (Intrinsic::ToAny, [x]) = (i, args) {
            self.made_dynamic(x, e.span, Some(e.ty), STORE);
            return self.unknown(e, to);
        }
        let boundary = match i {
            Intrinsic::Share | Intrinsic::Clone => {
                if let Some((x, rest)) = args.split_first() {
                    self.value(x, to);
                    rest.iter().for_each(|a| self.value(a, To::Drop));
                }
                return;
            }
            Intrinsic::Spawn | Intrinsic::SpawnHandled => {
                let why = self.why(e.span, Boundary::Spawn);
                // The promise; `spawn(p, { worker })`'s worker index is a plain value.
                if let Some((p, rest)) = args.split_first() {
                    self.spawned(p, why);
                    rest.iter().for_each(|a| self.value(a, To::Drop));
                }
                return self.unknown(e, to);
            }
            Intrinsic::SharedNew => Some(Boundary::Shared),
            Intrinsic::MutexNew | Intrinsic::RwLockNew => Some(Boundary::Mutex),
            Intrinsic::SlotPut | Intrinsic::SlotTransfer => Some(Boundary::Channel),
            Intrinsic::Transfer => Some(Boundary::Settle),
            _ => None,
        };
        let sink = match boundary {
            Some(b) => To::Flag(CROSSES, self.why(e.span, b)),
            None => STORE,
        };
        for a in args {
            let in_value = boundary == Some(Boundary::Shared)
                && !matches!(self.cx.ty.kind(a.ty), crate::hir::TyKind::FnPtr { .. });
            let sink = match sink {
                To::Flag(f, Some(w)) => To::Flag(f, Some(Why { in_value, ..w })),
                other => other,
            };
            self.value(a, sink);
        }
        self.unknown(e, to);
    }

    /// The operand of `spawn`: what goes to the new task. A promise that already started
    /// stays on its task, so only a call made here, or a task body, sends anything.
    fn spawned(&mut self, p: &Expr, why: Option<Why>) {
        let cross = To::Flag(CROSSES, why);
        match &p.kind {
            E::If { cond, then, els } => {
                self.value(cond, To::Drop);
                self.spawned(then, why);
                self.spawned(els, why);
            }
            E::Block(b) if b.stmts.is_empty() && b.value.is_some() => {
                if let Some(v) = &b.value {
                    self.spawned(v, why);
                }
            }
            E::Closure(_) => self.value(p, cross),
            E::Call {
                callee: Callee::Def(g, _),
                args,
            } => {
                for a in args {
                    self.value(a, cross);
                }
                // The task's result comes back to whoever awaits its handle on another task.
                if matches!(&self.cx.defs[g.0 as usize], Some(Def::Fn(_))) {
                    let n = self.ret_node(*g);
                    self.connect(n, To::Flag(CROSSES, why.map(Why::entered)));
                }
                self.value(p, To::Drop);
            }
            E::Call {
                callee: Callee::Indirect(f),
                args,
            } => {
                self.value(f, cross);
                for a in args {
                    self.value(a, cross);
                }
            }
            E::Call {
                callee: Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. },
                args,
            } => {
                for a in args {
                    self.value(a, cross);
                }
            }
            _ => self.value(p, To::Drop),
        }
    }
}
