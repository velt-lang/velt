//! When a record copy (`object_copies::record_widen`, a record converted to a record of a wider
//! value type) lives, and which code runs meanwhile: the functions its value reaches
//! ([`FlowGraph`]) and every function they may call. A write, a write through `any` or an
//! identity test elsewhere runs before the copy is made or after it is gone, so it cannot tell the
//! copy from the record; a typed write inside counts only if its record may be the copy or the
//! source. Where the calls can't be followed (a virtual, interface or type-parameter method call,
//! a function value of unknown origin) or other code may run meanwhile (an `await`, a generator),
//! the copy has no known lifetime and the type-level rule applies.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::copy_flow::{self, FlowGraph, Store};
use crate::ctx::Ctx;
use crate::hir::{
    Callee, Def, DefId, ExprKind as E, Intrinsic, LocalId, StmtKind as S, TyId, TyKind,
};

/// What a function does that matters to a copy's lifetime.
#[derive(Default)]
struct Facts {
    /// Functions it calls directly (constructors of the classes it makes among them), with where
    /// each argument comes from.
    calls: Vec<(DefId, Vec<Vec<Store>>)>,
    /// Where the function values it calls come from.
    indirect: Vec<Vec<Store>>,
    /// It calls a method that dispatches at run time, or suspends (`await`, a generator).
    opaque: bool,
    /// Its parameters, those of a function type with their local.
    params: Vec<Option<LocalId>>,
    /// A method: its call's first argument is the receiver, which its parameters leave out.
    method: bool,
}

/// The functions a value may be (`None`: any).
type Fns = Option<FxHashSet<DefId>>;

/// The lifetimes of the record copies of a checked program.
pub(super) struct Lives {
    graph: FlowGraph,
    facts: FxHashMap<DefId, Facts>,
    /// The function each span of interest is in.
    at: FxHashMap<Span, DefId>,
    /// For each record write (by span): its function and where its record comes from.
    receivers: FxHashMap<Span, (DefId, Vec<Store>)>,
    /// For each copy: where its source record comes from (empty: not found).
    sources: Vec<Vec<Store>>,
    /// Each function's own span, for a span no expression has (`function_at`).
    bodies: Vec<(Span, DefId)>,
    /// Was each copy's expression found? A copy the flow never saw has no known lifetime.
    found: Vec<bool>,
}

impl Lives {
    /// The flow of the program with `copies` (span and converted-to type), the functions of the
    /// spans in `spans`, and the records written at `writes`.
    pub(super) fn build(
        cx: &mut Ctx,
        copies: &[(Span, TyId)],
        spans: &FxHashSet<Span>,
        writes: &FxHashSet<Span>,
    ) -> Lives {
        let graph = FlowGraph::build(cx, copies);
        let record = cx.prelude_adt("Record");
        let mut ctors: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        for (i, d) in cx.defs.iter().enumerate() {
            if let Some(Def::Fn(f)) = d {
                if let Some(TyKind::Adt(c, _)) = f.self_ty.map(|t| cx.ty.kind(t).clone()) {
                    if f.name == "constructor" || f.name.ends_with("::constructor") {
                        ctors.entry(c).or_default().push(DefId(i as u32));
                    }
                }
            }
        }
        let methods: FxHashSet<DefId> = (0..cx.defs.len() as u32)
            .map(DefId)
            .filter(|&d| record.is_some() && cx.try_fn(d).is_some_and(|f| f.owner == record))
            .collect();
        let mut facts: FxHashMap<DefId, Facts> = FxHashMap::default();
        let mut at: FxHashMap<Span, DefId> = FxHashMap::default();
        let mut receivers = FxHashMap::default();
        let mut sources = vec![vec![]; copies.len()];
        let mut bodies = vec![];
        let mut found = vec![false; copies.len()];
        for (i, d) in cx.defs.iter_mut().enumerate() {
            let Some(Def::Fn(f)) = d else {
                continue;
            };
            let fd = DefId(i as u32);
            // The extent of its body's code in its own file (its span is its signature's).
            let mut body = Span {
                file: f.span.file,
                lo: f.span.lo,
                hi: f.span.hi,
            };
            // A function, or an optional one (`cmp?: (a: T, b: T) => number`).
            let is_fn = |t: TyId| {
                let t = cx.ty.opt_payload(t).unwrap_or(t);
                matches!(cx.ty.kind(t), TyKind::FnPtr { .. })
            };
            let params = (f.params.iter())
                .map(|p| is_fn(p.ty).then_some(p.local))
                .collect();
            let mut fx = Facts {
                opaque: f.is_async || f.is_generator,
                params,
                method: f.self_ty.is_some(),
                ..Facts::default()
            };
            crate::visit::exprs_mut(&mut f.body.block, &mut |e| {
                if e.span.file == body.file && e.span != Span::DUMMY {
                    body.lo = body.lo.min(e.span.lo);
                    body.hi = body.hi.max(e.span.hi);
                }
                if let Some(ci) = copies.iter().position(|&(s, t)| s == e.span && t == e.ty) {
                    found[ci] = true;
                }
                if spans.contains(&e.span) {
                    at.entry(e.span).or_insert(fd);
                }
                match &e.kind {
                    E::Call { callee, args } => match callee {
                        Callee::Def(m, _) => {
                            let srcs = args.iter().map(|a| copy_flow::sources(fd, a, copies));
                            fx.calls.push((*m, srcs.collect()));
                            if methods.contains(m) && writes.contains(&e.span) {
                                if let Some(r) = args.first() {
                                    let srcs = copy_flow::sources(fd, r, copies);
                                    receivers.insert(e.span, (fd, srcs));
                                }
                            }
                        }
                        Callee::Indirect(v) => fx.indirect.push(copy_flow::sources(fd, v, copies)),
                        Callee::Intrinsic(Intrinsic::Yield) => fx.opaque = true,
                        Callee::Intrinsic(_) => {}
                        Callee::Virtual { .. }
                        | Callee::Dyn { .. }
                        | Callee::ParamMethod { .. } => fx.opaque = true,
                    },
                    E::New { def, .. } => {
                        let made = ctors.get(def).into_iter().flatten();
                        fx.calls.extend(made.map(|&c| (c, vec![])));
                    }
                    E::Await(_) => fx.opaque = true,
                    E::Block(b) => {
                        let copy = copies.iter().position(|&(s, t)| s == e.span && t == e.ty);
                        let first = b.stmts.first().map(|s| &s.kind);
                        if let (Some(ci), Some(S::Let { init: Some(x), .. })) = (copy, first) {
                            sources[ci] = copy_flow::sources(fd, x, copies);
                        }
                    }
                    _ => {}
                }
            });
            facts.insert(fd, fx);
            bodies.push((body, fd));
        }
        Lives {
            graph,
            facts,
            at,
            receivers,
            sources,
            bodies,
            found,
        }
    }

    /// The functions that may run while copy `ci` lives (`None`: not known, or it reaches the
    /// heap), with the stores its value reaches. A function-typed parameter of a function that
    /// runs holds what the functions that run pass it (the functions the copy lives in hold what
    /// any caller passes).
    pub(super) fn live(&self, ci: usize) -> Option<(FxHashSet<DefId>, FxHashSet<Store>)> {
        if !self.found.get(ci).copied().unwrap_or(false) {
            return None;
        }
        let reach = self.graph.reach(&[Store::Copy(ci as u32)]);
        let mut roots: FxHashSet<DefId> = FxHashSet::default();
        for s in &reach {
            match *s {
                Store::Heap => return None,
                Store::Local(f, _) if f == copy_flow::GLOBAL => return None,
                Store::Local(f, _) | Store::Ret(f) => {
                    roots.insert(f);
                }
                Store::Copy(_) | Store::Fn(_) => {}
            }
        }
        let mut work: Vec<DefId> = roots.iter().copied().collect();
        // A function the copy is passed to is called by one it lives in, which passes its other
        // arguments too: only the others (where it is made, or returned to) take any caller's.
        let called: FxHashSet<DefId> = (roots.iter())
            .filter_map(|r| self.facts.get(r))
            .flat_map(|fx| fx.calls.iter().map(|c| c.0))
            .collect();
        roots.retain(|r| !called.contains(r));
        let mut live: FxHashSet<DefId> = FxHashSet::default();
        let mut passed: FxHashMap<(DefId, LocalId), Fns> = FxHashMap::default();
        while let Some(f) = work.pop() {
            live.insert(f);
            // An external function, or one without a body here: nothing of the program runs.
            let Some(fx) = self.facts.get(&f) else {
                continue;
            };
            if fx.opaque {
                return None;
            }
            for (d, args) in &fx.calls {
                let callee = self.facts.get(d);
                let ps = callee.map_or(&[][..], |c| &c.params[..]);
                let mut grew = !live.contains(d);
                // Arguments that don't line up with the parameters (a constructor's): unknown.
                let off =
                    usize::from(callee.is_some_and(|c| c.method) && args.len() == ps.len() + 1);
                let lined = args.len() == ps.len() + off;
                for (i, p) in ps.iter().enumerate() {
                    let Some(p) = *p else {
                        continue;
                    };
                    let v = match lined {
                        true => self.fn_values(f, &args[i + off], &roots, &passed),
                        false => None,
                    };
                    grew |= merge(
                        passed.entry((*d, p)).or_insert(Some(FxHashSet::default())),
                        v,
                    );
                }
                if grew {
                    work.push(*d);
                }
            }
            for srcs in &fx.indirect {
                let targets = self.fn_values(f, srcs, &roots, &passed)?;
                work.extend(targets.into_iter().filter(|t| !live.contains(t)));
            }
        }
        Some((live, reach))
    }

    /// The functions a value of `f` from `srcs` may be.
    fn fn_values(
        &self,
        f: DefId,
        srcs: &[Store],
        roots: &FxHashSet<DefId>,
        passed: &FxHashMap<(DefId, LocalId), Fns>,
    ) -> Fns {
        let mut out = FxHashSet::default();
        let tracked = |l: LocalId| {
            self.facts
                .get(&f)
                .is_some_and(|x| x.params.contains(&Some(l)))
        };
        let mut wide = vec![];
        for &s in srcs {
            match s {
                Store::Fn(d) => {
                    out.insert(d);
                }
                Store::Local(g, l) if g == f && tracked(l) => {
                    match passed.get(&(f, l)) {
                        Some(Some(fs)) => out.extend(fs.iter().copied()),
                        Some(None) => return None,
                        None => {}
                    }
                    if roots.contains(&f) {
                        wide.push(s);
                    }
                }
                Store::Heap => return None,
                _ => wide.push(s),
            }
        }
        for o in self.graph.origins(&wide) {
            match o {
                Store::Fn(d) => {
                    out.insert(d);
                }
                Store::Heap => return None,
                _ => {}
            }
        }
        Some(out)
    }

    /// The function `span` is in, if found.
    pub(super) fn function_at(&self, span: Span) -> Option<DefId> {
        if let Some(&f) = self.at.get(&span) {
            return Some(f);
        }
        // The innermost function whose span holds it (a closure's inside its function's).
        let holds = |s: &Span| s.file == span.file && s.lo <= span.lo && span.hi <= s.hi;
        (self.bodies.iter())
            .filter(|(s, _)| holds(s))
            .min_by_key(|(s, _)| s.hi - s.lo)
            .map(|&(_, f)| f)
    }

    /// May the record written at `span`, in a function of `live`, be copy `ci` (whose value
    /// reaches `reach`) or its source? Yes where the write or the source isn't found.
    pub(super) fn may_write(
        &self,
        ci: usize,
        span: Span,
        live: &FxHashSet<DefId>,
        reach: &FxHashSet<Store>,
    ) -> bool {
        let Some((f, recv)) = self.receivers.get(&span) else {
            return true;
        };
        if !live.contains(f) {
            return false;
        }
        let source = self.sources.get(ci).filter(|s| !s.is_empty());
        let Some(source) = source else {
            return true;
        };
        let from = self.graph.origins(recv);
        let src = self.graph.origins(source);
        from.iter().any(|s| reach.contains(s) || src.contains(s))
    }
}

/// `into` with `v` added: did it change?
fn merge(into: &mut Fns, v: Fns) -> bool {
    match (into.as_mut(), v) {
        (None, _) => false,
        (Some(_), None) => {
            *into = None;
            true
        }
        (Some(a), Some(b)) => {
            let n = a.len();
            a.extend(b);
            a.len() != n
        }
    }
}
