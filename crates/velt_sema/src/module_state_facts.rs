//! The [`Facts`] of every body for `check_locked_access` (module_state.rs), and a finished
//! unit's ([`StateFacts`], `crate::units`): its bodies never change, so a later unit's check
//! takes their facts in place of reading them again. What a call through a slot may run depends
//! on the classes of the whole program (a later unit's overrides and implementations too), so
//! a unit's facts keep those edges as written ([`Edge`]) and each check resolves them
//! ([`expand`]).

use rustc_hash::{FxHashMap, FxHashSet};

use super::locked_props;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as E, TyId, TyKind};

/// What one body does with module state: its own first use, and the code it may run (calls,
/// closures it makes, functions it takes as values, the methods a dynamic call may pick, the
/// initializers a `new` or a constant read evaluates); and the types of the values it handles,
/// whose class hooks it may run without a call (`replaced_holders::implicit`).
#[derive(Clone)]
pub(super) struct Facts {
    pub(super) direct: Option<String>,
    pub(super) vars: Vec<DefId>, // every module state variable it uses (`script_locals`)
    pub(super) made: Vec<DefId>, // the closures and functions it makes as values (same)
    pub(super) values: bool,     // it calls a function value whose body is not known (same)
    pub(super) edges: Vec<DefId>,
    pub(super) tys: FxHashSet<TyId>,
    /// It resumes a generator: any generator body may run.
    pub(super) resumes: bool,
    /// It may reach a function's properties by itself (locked_props.rs).
    pub(super) props: bool,
    /// The interface slots it calls through (`(iface, slot)`): their implementations are its
    /// edges only when module state is checked; the walk for properties goes through the slot
    /// once instead (locked_props.rs), so many calls of many implementations stay linear.
    pub(super) slots: Vec<(DefId, u32)>,
    /// Its edges as written, until [`expand`] makes them `edges`.
    written: Vec<Edge>,
}

/// An edge of a body as written: a definition it runs, or the methods a call through a slot may
/// run.
#[derive(Clone, Copy)]
enum Edge {
    Def(DefId),
    /// A virtual call through slot `.1` of class `.0`.
    Virtual(DefId, u32),
    /// A call through slot `.1` of interface `.0`: its implementations are edges only when
    /// module state is checked ([`Facts::slots`]).
    Iface(DefId, u32),
}

/// A finished unit's facts (`Ctx::state_facts`): its bodies' and initializers' facts with their
/// edges as written, and the callbacks of its `with` calls, those of bodies and those of
/// initializers.
#[derive(Clone, Default)]
pub(crate) struct StateFacts {
    facts: FxHashMap<DefId, Facts>,
    fn_callbacks: Vec<DefId>,
    init_callbacks: Vec<DefId>,
}

/// The facts of every body and initializer, with the edges resolved.
pub(super) struct Scanned {
    pub(super) facts: FxHashMap<DefId, Facts>,
    /// The `with` calls outside the standard library: callback, where, the method.
    pub(super) withs: Vec<(hir::Expr, velt_common::Span, &'static str)>,
    /// The callbacks of every `with`, `read` and `write` (locked_props.rs).
    pub(super) callbacks: Vec<DefId>,
    pub(super) proxies: locked_props::Proxies,
}

/// The facts of the bodies and initializers: a finished unit's from its summary, the unit's own
/// read here. std's stage keeps its facts for the app's.
pub(super) fn scan(cx: &mut Ctx, dispatch: &locked_props::Dispatch) -> Scanned {
    use crate::units::Stage;
    let app = cx.units_stage == Stage::App;
    let (mut facts, mut callbacks, mut init_callbacks) = match app {
        true => {
            let s = &cx.unit_sums.state_facts;
            (
                s.facts.clone(),
                s.fn_callbacks.clone(),
                s.init_callbacks.clone(),
            )
        }
        false => Default::default(),
    };
    let mut withs = vec![];
    let mut proxies = locked_props::Proxies::default();
    for d in cx.unit_fns() {
        let std = cx.modules[cx.fn_info(d).module].is_std;
        let Some(hir::Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut v = FactsOf::new(cx, std);
        if !std {
            v.facts.tys.extend(f.body.locals.iter().map(|l| l.ty));
        }
        crate::visit::block(&mut f.body.block, &mut v);
        let (fd, ws) = (v.facts, v.withs);
        callbacks.extend(v.callbacks);
        proxies.add(v.proxies);
        cx.defs[d.0 as usize] = Some(hir::Def::Fn(f));
        if !std {
            withs.extend(ws);
        }
        facts.insert(d, fd);
    }
    // Initializers evaluated where they are used (a constant's at each read, an object type's
    // field initializers at each `new` or literal): each a node of its own, under the
    // constant's or the type's `DefId`.
    let from = if app { cx.unit_base as usize } else { 0 };
    for i in from..cx.info.len() {
        let (module, inits): (usize, Vec<hir::Expr>) = match &cx.info[i] {
            DefInfo::Global(g) if !g.stored => (g.module, g.init.iter().cloned().collect()),
            DefInfo::Adt(a) => (
                a.module,
                a.fields
                    .iter()
                    .filter_map(|f| f.default.clone())
                    .chain(a.redefined.iter().filter_map(|r| r.init.clone()))
                    .collect(),
            ),
            _ => continue,
        };
        if inits.is_empty() {
            continue;
        }
        let std = cx.modules.get(module).is_some_and(|m| m.is_std);
        let mut v = FactsOf::new(cx, std);
        for mut x in inits {
            crate::visit::expr(&mut x, &mut v);
        }
        let (fd, ws) = (v.facts, v.withs);
        init_callbacks.extend(v.callbacks);
        if !std {
            withs.extend(ws);
        }
        facts.insert(DefId(i as u32), fd);
    }
    if cx.units_stage == Stage::Std {
        cx.unit_sums.state_facts = StateFacts {
            facts: facts.clone(),
            fn_callbacks: callbacks.clone(),
            init_callbacks: init_callbacks.clone(),
        };
    }
    callbacks.extend(init_callbacks);
    for f in facts.values_mut() {
        expand(cx, dispatch, f);
    }
    Scanned {
        facts,
        withs,
        callbacks,
        proxies,
    }
}

/// Make `f`'s written edges its edges, with the methods each call through a slot may run.
fn expand(cx: &Ctx, dispatch: &locked_props::Dispatch, f: &mut Facts) {
    for e in std::mem::take(&mut f.written) {
        match e {
            Edge::Def(d) => f.edges.push(d),
            Edge::Virtual(class, slot) => f.edges.extend(dispatch.virtual_methods(class, slot)),
            Edge::Iface(iface, slot) if !cx.module_slots.is_empty() => {
                f.edges.extend(dispatch.iface_methods(cx, iface, slot))
            }
            Edge::Iface(..) => {}
        }
    }
}

/// Is `e` a closure or a named function written in place (an optional parameter's `Some`, or
/// either branch of a conditional): a function value whose body the visit sees?
fn written_fn(e: &hir::Expr) -> bool {
    match &e.kind {
        E::Closure(_) | E::FnRef(..) => true,
        E::WrapSome(x) | E::Upcast(x) => written_fn(x),
        E::If { then, els, .. } => written_fn(then) && written_fn(els),
        E::Block(b) if b.stmts.is_empty() => b.value.as_deref().is_some_and(written_fn),
        _ => false,
    }
}

/// Collects a body's [`Facts`] and the callbacks of its `with` calls.
struct FactsOf<'a, 'm> {
    cx: &'a Ctx<'m>,
    std: bool,
    facts: Facts,
    withs: Vec<(hir::Expr, velt_common::Span, &'static str)>,
    /// The callbacks of every `with`, `read` and `write` written as a closure or a named
    /// function, also the standard library's (locked_props.rs).
    callbacks: Vec<DefId>,
    proxies: locked_props::Proxies,
}

impl<'a, 'm> FactsOf<'a, 'm> {
    fn new(cx: &'a Ctx<'m>, std: bool) -> Self {
        FactsOf {
            cx,
            std,
            facts: Facts {
                direct: None,
                vars: vec![],
                made: vec![],
                values: false,
                edges: vec![],
                tys: FxHashSet::default(),
                resumes: false,
                props: false,
                slots: vec![],
                written: vec![],
            },
            withs: vec![],
            callbacks: vec![],
            proxies: Default::default(),
        }
    }

    fn unknown(&mut self, what: &str) {
        // The standard library only calls the function values it is given: those are made, or
        // taken as values, by the code that passes them, where they count (`passed`).
        if !self.std {
            self.facts.props = true;
            self.facts.values = true;
            if self.facts.direct.is_none() {
                self.facts.direct = Some(what.into());
            }
        }
    }

    /// Arguments of a call into the standard library or an intrinsic: one that is or holds an
    /// `any` may have its members read, written or listed there (locked_props.rs).
    fn dynamic_args(&mut self, args: &[hir::Expr]) {
        if !self.facts.props {
            let cx = self.cx;
            self.facts.props = args.iter().any(|a| locked_props::holds_dynamic(cx, a.ty));
        }
    }

    /// The arguments of a call into code this analysis does not follow (the standard library,
    /// an intrinsic, or a method a dynamic call picks, which may be std's): a function value it
    /// is handed may run, and only a closure or a named function written there is known.
    fn passed(&mut self, args: &[hir::Expr]) {
        for a in args {
            if !written_fn(a) {
                self.fn_values_in(a.ty, 0);
            }
        }
    }

    /// The function values a value of type `t` holds by itself (not behind an object): a
    /// closure's type names its body, any other is not known.
    fn fn_values_in(&mut self, t: TyId, depth: u32) {
        if depth > 8 {
            return;
        }
        match self.cx.ty.kind(t) {
            TyKind::Closure(c) => self.facts.written.push(Edge::Def(*c)),
            TyKind::FnPtr { .. } => self.unknown(
                "passes a function value whose body is not known here to code that may call it",
            ),
            TyKind::Option(x, _) | TyKind::Array(x) | TyKind::Shared(x) => {
                self.fn_values_in(*x, depth + 1)
            }
            TyKind::Tuple(xs) => {
                for &x in xs {
                    self.fn_values_in(x, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Is `d` (a function or an object type) the standard library's?
    fn is_std_def(&self, d: DefId) -> bool {
        let module = match self.cx.info.get(d.0 as usize) {
            Some(DefInfo::Fn(f)) => f.module,
            Some(DefInfo::Adt(a)) => a.module,
            _ => return true,
        };
        self.cx.modules.get(module).is_none_or(|m| m.is_std)
    }

    /// The methods a call through slot `slot` of interface `iface` may run.
    fn iface_methods(&mut self, iface: DefId, slot: u32) {
        self.facts.slots.push((iface, slot));
        self.facts.written.push(Edge::Iface(iface, slot));
    }

    /// The methods a virtual call through slot `slot` of class `class` may run (also subclasses').
    fn virtual_methods(&mut self, class: DefId, slot: u32) {
        self.facts.written.push(Edge::Virtual(class, slot));
    }
}

impl crate::visit::VisitMut for FactsOf<'_, '_> {
    fn expr(&mut self, e: &mut hir::Expr) {
        if !self.std {
            self.facts.tys.insert(e.ty);
            self.proxies.note(e);
        }
        match &e.kind {
            E::Global(d) => match self.cx.global(*d) {
                Some(g) if g.stored => {
                    if self.facts.direct.is_none() {
                        self.facts.direct = Some(format!("uses `{}`", g.name));
                    }
                    self.facts.vars.push(*d);
                }
                // A constant's initializer runs at each read.
                Some(g)
                    if g.init
                        .as_ref()
                        .is_some_and(|x| !matches!(x.kind, E::Lit(_))) =>
                {
                    self.facts.written.push(Edge::Def(*d))
                }
                _ => {}
            },
            E::Closure(c) | E::FnRef(c, _) => {
                self.facts.written.push(Edge::Def(*c));
                self.facts.made.push(*c);
            }
            E::New { def, args, .. } => {
                let ctor = self.cx.adt(*def).and_then(|a| a.ctor);
                self.facts.written.extend(ctor.map(Edge::Def));
                // Its field initializers.
                self.facts.written.push(Edge::Def(*def));
                if ctor.is_none_or(|c| self.is_std_def(c)) {
                    self.passed(args);
                    if !ctor.is_some_and(|c| locked_props::has_body(self.cx, c)) {
                        self.dynamic_args(args);
                    }
                }
            }
            E::AdtLit { def, .. } => self.facts.written.push(Edge::Def(*def)),
            E::Call { callee, args } => match callee {
                hir::Callee::Def(f, _) => {
                    self.facts.written.push(Edge::Def(*f));
                    if self.is_std_def(*f) {
                        self.passed(args);
                        // A body the walk follows decides by itself (its edge); a runtime
                        // extern may list or print what it gets.
                        if !locked_props::has_body(self.cx, *f) {
                            self.dynamic_args(args);
                        }
                    }
                }
                hir::Callee::Indirect(_) => self.unknown("calls a function value"),
                hir::Callee::Virtual { slot } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    match args.first().and_then(|a| self.cx.class_of(a.ty)) {
                        Some((c, _)) => self.virtual_methods(c, *slot),
                        None => self.unknown("calls a function value"),
                    }
                }
                hir::Callee::Dyn { slot, .. } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    let recv = args.first().map(|a| self.cx.ty.kind(a.ty).clone());
                    match recv {
                        Some(TyKind::Dyn(i, _)) => self.iface_methods(i, *slot),
                        _ => self.unknown("calls a function value"),
                    }
                }
                hir::Callee::ParamMethod { iface, slot, .. } => {
                    self.passed(args);
                    self.dynamic_args(args);
                    self.iface_methods(*iface, *slot)
                }
                hir::Callee::Intrinsic(
                    i @ (hir::Intrinsic::MutexWith | hir::Intrinsic::LockRead),
                ) => {
                    if let [m, cb] = args.as_slice() {
                        self.passed(std::slice::from_ref(m));
                        let method = match (i, self.cx.is_rwlock(m.ty)) {
                            (hir::Intrinsic::LockRead, _) => "read",
                            (_, true) => "write",
                            _ => "with",
                        };
                        self.withs.push((cb.clone(), cb.span, method));
                        match &cb.kind {
                            E::Closure(c) | E::FnRef(c, _) => self.callbacks.push(*c),
                            // Lowering takes the module lock for it: the code around it may
                            // not hold a `Mutex` then (locked_props.rs).
                            _ => self.facts.props = true,
                        }
                    }
                }
                hir::Callee::Intrinsic(i) => {
                    self.facts.resumes |= crate::replaced_holders::implicit::resumes(*i);
                    self.passed(args);
                    self.dynamic_args(args);
                }
            },
            _ => {}
        }
    }
}
