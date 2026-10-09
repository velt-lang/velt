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

use std::collections::HashMap;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo};
use crate::hir::{
    Block, Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, PatKind, Stmt,
    StmtKind as S, TyId,
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Node {
    /// The closure literal `def` (its value where it is created).
    Lit(DefId),
    Local(DefId, LocalId),
    /// What function `def` returns.
    Ret(DefId),
    /// Argument `1` of the call through a function value numbered `0` (`Graph::indirect`).
    Arg(u32, u32),
}

/// Where a value leaves the task that made it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Boundary {
    /// Spawned: a closure literal, the callee or an argument of a spawned call.
    Spawn,
    /// The handler of an HTTP server (requests run on every core).
    Handler,
    /// `shared(...)`.
    Shared,
    /// The value of a `Mutex`, which other threads lock once it is shared.
    Mutex,
    /// A channel send.
    Channel,
    /// A value that settles a promise, which another task may await.
    Settle,
    /// An argument of a call through a function value, an interface or an overridden method:
    /// the callee is unknown and may keep it.
    FnValue,
}

/// Why a value crosses: the boundary, where (in user code when the boundary is in the
/// standard library: the call into it), and the type that carries it there when the value
/// is reached through the heap.
#[derive(Clone, Copy, Debug)]
pub(super) struct Why {
    pub span: Span,
    pub boundary: Boundary,
    /// `span` is in the standard library.
    pub std: bool,
    /// A value of this type reaches the boundary, and the closure is stored in it.
    pub via: Option<TyId>,
}

impl Why {
    /// `self`, seen from an argument passed at `site` (`(span, in std)`): a boundary inside the
    /// standard library is reported at the user's call into it.
    pub(super) fn through(self, site: Option<(Span, bool)>) -> Why {
        match site {
            Some((span, false)) if self.std => Why {
                span,
                std: false,
                ..self
            },
            _ => self,
        }
    }
}

/// Node flags.
pub(super) const CROSSES: u8 = 1;
pub(super) const INDIRECT: u8 = 2;
pub(super) const STORED: u8 = 4;

/// The call passing an argument into a parameter: its span, and whether it is in std.
pub(super) type Site = Option<(Span, bool)>;

#[derive(Default)]
pub(super) struct Graph {
    pub ids: HashMap<Node, usize>,
    pub nodes: Vec<Node>,
    pub tys: Vec<TyId>,
    /// Per node: the nodes whose values flow into it, each with the call passing them (an
    /// argument flowing into a parameter: the call's span and whether it is in std).
    pub srcs: Vec<Vec<(usize, Site)>>,
    /// Per node: may it hold a value of unknown origin?
    pub unknown: Vec<bool>,
    /// Per node: the types of the values of unknown origin flowing into it, as written where
    /// they flow in (a generic parameter's node gets its callers' concrete types).
    pub inflows: Vec<Vec<(TyId, Site)>>,
    /// Calls through a function value: the callee's type, the argument nodes, and the call
    /// (span, in std). A closure whose parameter crosses makes the matching argument cross.
    pub indirect: Vec<(TyId, Vec<usize>, Span, bool)>,
    /// The function creating each closure literal.
    pub parent: HashMap<DefId, DefId>,
    /// The type arguments of every direct call of each generic function, with the function
    /// making the call (its own generic parameters may appear in them).
    pub insts: HashMap<DefId, Vec<(DefId, Vec<TyId>)>>,
    /// Nodes whose values reach a flag's sink directly.
    pub seeds: Vec<(usize, u8, Option<Why>)>,
    /// Values of unknown origin that cross (or reach an unknown callee, for function values).
    pub roots: Vec<(TyId, Why)>,
    /// Per closure literal node: its capture locals' nodes.
    pub captures: HashMap<usize, Vec<usize>>,
    /// Per function: the types of the function values it reads from a field or an element
    /// (to call them or pass them on).
    pub fn_reads: HashMap<DefId, Vec<TyId>>,
    /// Per function: the functions it calls directly.
    pub callees: HashMap<DefId, Vec<DefId>>,
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
    /// Used here (read, compared, called, borrowed): nowhere else.
    Drop,
    /// Into a node, passed by the call at the span (an argument), if any.
    Node(usize, Option<(Span, bool)>),
    /// Into a sink: a boundary (`CROSSES`), an unknown callee (`INDIRECT`) or the heap
    /// (`STORED`).
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
        let std = cx.scopes[info.module].is_std;
        let mut w = Walk {
            cx,
            g: &mut g,
            d,
            f,
            std,
        };
        let ret = w.ret_node(d);
        w.block(&f.body.block, To::Node(ret, None));
    }
    g
}

struct Walk<'a, 'm> {
    cx: &'a Ctx<'m>,
    g: &'a mut Graph,
    d: DefId,
    f: &'a FnDef,
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
            std: self.std,
            via: None,
        })
    }

    /// Values of node `n` go to `to`.
    fn connect(&mut self, n: usize, to: To) {
        match to {
            To::Drop => {}
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
            }
            To::Flag(CROSSES, Some(why)) => self.g.roots.push((e.ty, why)),
            To::Flag(INDIRECT, Some(why)) if super::types::fn_like(self.cx, e.ty) => {
                self.g.roots.push((e.ty, why))
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
    fn bind(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Binding(l, _) => {
                let n = self.local(*l);
                self.g.unknown[n] = true;
            }
            PatKind::Variant { args, .. } | PatKind::Tuple(args) | PatKind::Or(args) => {
                args.iter().for_each(|a| self.bind(a))
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, a)| self.bind(a)),
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|a| self.bind(a));
                if let Some(l) = rest {
                    let n = self.local(*l);
                    self.g.unknown[n] = true;
                }
            }
            PatKind::Some(a) => self.bind(a),
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
        }
    }

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
                self.value(scrutinee, To::Drop);
                for a in arms {
                    self.bind(&a.pat);
                    if let Some(g) = &a.guard {
                        self.value(g, To::Drop);
                    }
                    self.value(&a.body, to);
                }
            }
            E::WrapSome(x)
            | E::Cast(x)
            | E::Upcast(x)
            | E::Downcast(x)
            | E::UnwrapSome(x, _)
            | E::Await(x)
            | E::ToDyn { expr: x, .. }
            | E::UnwrapVariant { expr: x, .. } => self.value(x, to),
            E::Logical { lhs, rhs, .. } => {
                self.value(lhs, to);
                self.value(rhs, to);
            }
            E::Call { callee, args } => self.call(e, callee, args, to),
            E::New { def, args, .. } => {
                let ctor = match &self.cx.info[def.0 as usize] {
                    DefInfo::Adt(a) => a.ctor,
                    _ => None,
                };
                self.pass_to_params(ctor, args, e.span);
                self.unknown(e, to);
            }
            E::AdtLit { fields: xs, .. }
            | E::ArrayLit(xs)
            | E::Tuple(xs)
            | E::Variant { args: xs, .. } => {
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
                self.fn_read(e.ty);
                self.value(base, To::Drop);
                self.unknown(e, to);
            }
            E::Index { base, index, .. } => {
                self.fn_read(e.ty);
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
            E::Throw(x) => self.value(x, STORE),
            E::Lit(_) | E::Global(_) | E::FnRef(..) => self.unknown(e, to),
        }
    }

    /// A function value of type `ty` (or `ty | null`) read from a field or an element.
    fn fn_read(&mut self, ty: TyId) {
        let t = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        if matches!(self.cx.ty.kind(t), crate::hir::TyKind::FnPtr { .. }) {
            self.g.fn_reads.entry(self.d).or_default().push(t);
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
            self.value(a, To::Node(n, Some((span, self.std))));
        }
    }

    fn call(&mut self, e: &Expr, callee: &Callee, args: &[Expr], to: To) {
        match callee {
            Callee::Def(g, targs) => {
                self.g.callees.entry(self.d).or_default().push(*g);
                if !targs.is_empty() {
                    let site = (self.d, targs.clone());
                    self.g.insts.entry(*g).or_default().push(site);
                }
                let direct = matches!(&self.cx.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.params.len() >= args.len());
                self.pass_to_params(Some(*g), args, e.span);
                if direct {
                    let n = self.ret_node(*g);
                    self.connect(n, to);
                } else {
                    self.unknown(e, to);
                }
            }
            Callee::Indirect(f) => {
                self.value(f, To::Drop);
                let why = self.why(e.span, Boundary::FnValue);
                let call = self.g.indirect.len() as u32;
                let mut nodes = vec![];
                for (i, a) in args.iter().enumerate() {
                    let n = self.g.node(Node::Arg(call, i as u32), a.ty);
                    self.value(a, To::Node(n, None));
                    self.g.seeds.push((n, INDIRECT, why));
                    nodes.push(n);
                }
                self.g.indirect.push((f.ty, nodes, e.span, self.std));
                self.unknown(e, to);
            }
            Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                let why = self.why(e.span, Boundary::FnValue);
                for (i, a) in args.iter().enumerate() {
                    let to = if i == 0 {
                        To::Drop
                    } else {
                        To::Flag(INDIRECT, why)
                    };
                    self.value(a, to);
                }
                self.unknown(e, to);
            }
            Callee::Intrinsic(i) => self.intrinsic(e, *i, args, to),
        }
    }

    fn intrinsic(&mut self, e: &Expr, i: Intrinsic, args: &[Expr], to: To) {
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
                for a in args {
                    self.spawned(a, why);
                }
                return self.unknown(e, to);
            }
            Intrinsic::HttpHandler => Some(Boundary::Handler),
            Intrinsic::SharedNew => Some(Boundary::Shared),
            Intrinsic::MutexNew => Some(Boundary::Mutex),
            Intrinsic::ChanSend | Intrinsic::ChanTrySend => Some(Boundary::Channel),
            Intrinsic::Transfer => Some(Boundary::Settle),
            _ => None,
        };
        let sink = match boundary {
            Some(b) => To::Flag(CROSSES, self.why(e.span, b)),
            None => STORE,
        };
        for a in args {
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
                    self.connect(n, cross);
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
