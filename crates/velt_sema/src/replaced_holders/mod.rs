//! An assignment into an object literal stored inline in its parent, whose right-hand side may
//! replace that object (#876): `o.inner.v = g()` where `g` runs `o.inner = { … }`. JavaScript
//! evaluates the target first, so the old object (which nothing refers to any more) gets the
//! value and the new one keeps its own. Velt stores an object literal nothing else refers to
//! inside its holder, so the target is formed after the right-hand side and the write would
//! land in the new object. That silent difference is an error instead; the fix computes the
//! value first (`const v = g(); o.inner.v = v;`), which TypeScript runs the same way.
//!
//! Only where the difference can happen:
//! - the target's object is an object literal type (`AdtKind::Anon`, not one that contains
//!   itself, which is always a counted box) read as a field of another value;
//! - nothing in the program shares a value of that type (`Intrinsic::Share`, a capture that
//!   shares, a cell, or a shared value holding it inline): a shared object is a counted box,
//!   and lowering writes to the object the target named before the right-hand side ran. The
//!   exemption is binding: such a type goes to `hir::Program::counted_objects`, which lowering
//!   counts whether or not the code sharing it is lowered;
//! - the right-hand side may assign a whole value of the type of the target's object or of a
//!   holder above it (up to the nearest class object or shared object, which lowering pins):
//!   directly, or in a function it calls (transitively). A generic function's field of a
//!   type-parameter type is substituted with the call's type arguments; where those are not
//!   known, it may be any type the program uses as a type argument. A call through a function
//!   value may run any function the program uses as a value, except that a callback written in
//!   a call's arguments is followed instead, and a function calling its own parameter runs what
//!   its callers passed. A method call through a vtable or an interface runs the methods of
//!   that name (and the interface's implementations), and an `await` anything. A right-hand
//!   side that only reads, computes and builds values compiles.

use std::collections::HashSet;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{
    AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, LocalId, TyId, TyKind,
};
use crate::visit;

mod fix;

/// Most (function, type arguments) pairs [`Program::may_assign`] visits before it gives up and
/// answers "may".
const MAX_VISITS: usize = 20_000;

/// One assignment whose target lies in an inline object literal.
struct Candidate {
    span: Span,
    value_span: Span,
    /// What the right-hand side runs.
    value: Effects,
    /// The types whose assignment replaces the object the target lies in: its type first, then
    /// its holders', up to a class object (pinned by lowering).
    path: Vec<TyId>,
    /// The holder as written (`o.inner`), its parent (`o`) and the target (`o.inner.v`).
    holder_text: String,
    parent_text: String,
    target_text: String,
}

/// The type arguments a called function runs with.
#[derive(Clone, Debug)]
enum Frame {
    /// The caller's (a closure written in its body).
    Inherit,
    /// These, in terms of the caller's.
    Known(Vec<TyId>),
    /// Not known (an interface method's implementation).
    Unknown,
}

/// What running an expression may assign directly, and what it calls.
#[derive(Default)]
struct Effects {
    /// Types of the places it assigns whole values to.
    assigns: HashSet<TyId>,
    /// Types of the fields it assigns whose type mentions a type parameter.
    generic: Vec<TyId>,
    /// Functions it calls directly (`new` calls the constructor, an interface call each
    /// implementation), and the callbacks written in the arguments of its calls.
    calls: Vec<(DefId, Frame)>,
    /// It calls a function value other than its own parameters, or hands a call one other
    /// than those and the callbacks written there: any function the program uses as a value
    /// may run.
    indirect: bool,
    /// It calls methods through a vtable or an interface: the methods of these names (`None`:
    /// any method).
    methods: Vec<Option<String>>,
    /// It awaits: anything in the program may run meanwhile. (Only an `await` in the
    /// right-hand side itself counts: a function it calls runs up to its first `await` and
    /// returns, and nothing else runs until the right-hand side is done.)
    anything: bool,
    /// Functions it uses as values (closures it creates, functions it refers to).
    values: Vec<DefId>,
    /// Type arguments it passes (to calls, `new`, function references).
    type_args: Vec<TyId>,
}

/// Is `t` a function value?
fn is_fn(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::FnPtr { .. } | TyKind::Closure(_))
}

fn has_params(cx: &Ctx, t: TyId) -> bool {
    let mut ps = vec![];
    crate::types::collect_params(&cx.ty, t, &mut ps);
    !ps.is_empty() || matches!(cx.ty.kind(t), TyKind::Param(_))
}

/// The member name of a method (`greet` for `User.greet`).
fn member(name: &str) -> &str {
    let name = name.rsplit("::").next().unwrap_or(name);
    name.rsplit('.').next().unwrap_or(name)
}

impl Effects {
    fn runs_code(&self) -> bool {
        !self.assigns.is_empty()
            || !self.generic.is_empty()
            || !self.calls.is_empty()
            || self.indirect
            || !self.methods.is_empty()
            || self.anything
    }

    /// `params`: the function's own parameters of function type, never reassigned.
    fn note(&mut self, cx: &Ctx, params: &HashSet<LocalId>, e: &Expr) {
        let own = |x: &Expr| matches!(x.kind, E::Local(l, _) if params.contains(&l));
        match &e.kind {
            E::Assign { place, .. } => {
                // Only a field holds an object literal inline (an element replaced is #820).
                if matches!(place.kind, E::Field { .. }) && has_params(cx, place.ty) {
                    self.generic.push(place.ty);
                } else {
                    self.assigns.insert(place.ty);
                }
            }
            E::Call { callee, args } => {
                self.callbacks(cx, &own, args);
                match callee {
                    Callee::Def(d, targs) => {
                        self.calls.push((*d, Frame::Known(targs.clone())));
                        self.type_args.extend(targs);
                    }
                    Callee::Intrinsic(_) => {}
                    Callee::Indirect(f) => self.indirect |= !own(f),
                    Callee::Virtual { slot } => {
                        let m = args.first().and_then(|r| match cx.ty.kind(r.ty) {
                            TyKind::Adt(d, _) => match cx.info.get(d.0 as usize) {
                                Some(DefInfo::Adt(a)) => a.vtable.get(*slot as usize).copied(),
                                _ => None,
                            },
                            _ => None,
                        });
                        let name = m.and_then(|m| match cx.info.get(m.0 as usize) {
                            Some(DefInfo::Fn(f)) => Some(member(&f.name).to_string()),
                            _ => None,
                        });
                        self.methods.push(name);
                    }
                    Callee::Dyn { slot } => {
                        let iface = args.first().and_then(|r| match cx.ty.kind(r.ty) {
                            TyKind::Dyn(i, _) => Some(*i),
                            _ => None,
                        });
                        self.iface_call(cx, iface, *slot);
                    }
                    Callee::ParamMethod {
                        iface,
                        iface_args,
                        slot,
                        method_type_args,
                    } => {
                        self.type_args.extend(iface_args);
                        self.type_args.extend(method_type_args);
                        self.iface_call(cx, Some(*iface), *slot);
                    }
                }
            }
            E::New {
                def,
                type_args,
                args,
            } => {
                self.callbacks(cx, &own, args);
                self.type_args.extend(type_args);
                match cx.info.get(def.0 as usize) {
                    Some(DefInfo::Adt(a)) => self
                        .calls
                        .extend(a.ctor.map(|c| (c, Frame::Known(type_args.clone())))),
                    _ => self.anything = true,
                }
            }
            E::Closure(d) => self.values.push(*d),
            E::FnRef(d, targs) => {
                self.values.push(*d);
                self.type_args.extend(targs);
            }
            E::Await(_) => self.anything = true,
            _ => {}
        }
    }

    /// The function values among a call's arguments: a callback written there runs (it is
    /// followed), the function's own parameter is its callers' business, and anything else may
    /// be any function value.
    fn callbacks(&mut self, cx: &Ctx, own: &dyn Fn(&Expr) -> bool, args: &[Expr]) {
        for a in args.iter().filter(|a| is_fn(cx, a.ty)) {
            match &a.kind {
                E::Closure(d) => self.calls.push((*d, Frame::Inherit)),
                E::FnRef(d, targs) => self.calls.push((*d, Frame::Known(targs.clone()))),
                _ if own(a) => {}
                _ => self.indirect = true,
            }
        }
    }

    /// A call of method `slot` of interface `iface`: each implementation and the default, and
    /// the methods of its name (overrides of a class's implementation).
    fn iface_call(&mut self, cx: &Ctx, iface: Option<DefId>, slot: u32) {
        let Some(iface) = iface else {
            self.methods.push(None);
            return;
        };
        for imp in cx.impls.iter().filter(|i| i.iface == iface) {
            if let Some(&m) = imp.methods.get(slot as usize) {
                self.calls.push((m, Frame::Unknown));
            }
        }
        let name = match cx.info.get(iface.0 as usize) {
            Some(DefInfo::Iface(i)) => i.methods.get(slot as usize).map(|m| {
                if let Some(d) = m.default {
                    self.calls.push((d, Frame::Unknown));
                }
                m.name.clone()
            }),
            _ => None,
        };
        self.methods.push(name);
    }
}

/// Report the assignments the module docs describe.
pub(crate) fn check(cx: &mut Ctx) {
    let mut fns: Vec<Option<Effects>> = (0..cx.defs.len()).map(|_| None).collect();
    let (mut values, mut methods) = (Vec::new(), Vec::new());
    let mut type_args: Vec<TyId> = Vec::new();
    let mut shared: Vec<TyId> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (i, slot) in fns.iter_mut().enumerate() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        for l in &f.body.locals {
            if l.boxed {
                shared.push(l.ty);
            }
        }
        let names: Vec<String> = f.body.locals.iter().map(|l| l.name.clone()).collect();
        let captures: Vec<_> = f
            .captures
            .iter()
            .filter(|c| c.share)
            .map(|c| c.inner)
            .collect();
        for c in captures {
            if let Some(l) = f.body.locals.get(c.0 as usize) {
                shared.push(l.ty);
            }
        }
        let params: HashSet<LocalId> = f
            .params
            .iter()
            .filter(|p| is_fn(cx, p.ty))
            .filter(|p| {
                let l = f.body.locals.get(p.local.0 as usize);
                l.is_some_and(|l| !l.mutable && !l.boxed)
            })
            .map(|p| p.local)
            .collect();
        let mut fx = Effects::default();
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            fx.note(cx, &params, e);
            match &e.kind {
                E::Call {
                    callee: Callee::Intrinsic(Intrinsic::Share),
                    ..
                } => shared.push(e.ty),
                E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                    candidates.extend(candidate(cx, &names, e.span, place, value));
                }
                _ => {}
            }
        });
        values.extend(fx.values.iter().copied());
        type_args.append(&mut fx.type_args);
        if f.self_ty.is_some() {
            methods.push((member(&f.name).to_string(), DefId(i as u32)));
        }
        *slot = Some(fx);
        cx.defs[i] = Some(Def::Fn(f));
    }
    if candidates.is_empty() {
        return;
    }
    let shared = close_shared(cx, shared);
    let type_args = type_args_closure(cx, type_args);
    let program = Program {
        fns,
        values,
        methods,
        type_args,
        shared,
    };
    let mut counted = Vec::new();
    for c in candidates {
        // The holders lowering does not pin: up to the nearest shared object.
        let open = c
            .path
            .iter()
            .take_while(|t| !program.pinned(cx, **t))
            .count();
        if program.may_assign(cx, &c.value, &c.path[..open]) {
            fix::report(cx, &c);
        } else if open < c.path.len() && program.may_assign(cx, &c.value, &c.path) {
            // Accepted because a holder is shared: lowering must count it.
            counted.push(c.path[open]);
        }
    }
    for t in counted {
        if !cx.counted_objects.contains(&t) {
            cx.counted_objects.push(t);
        }
    }
}

/// Every type nested in the type arguments the program passes and in those of the types it
/// uses.
fn type_args_closure(cx: &Ctx, mut work: Vec<TyId>) -> HashSet<TyId> {
    for i in 0..cx.ty.table.len() {
        match cx.ty.kind(TyId(i as u32)) {
            TyKind::Adt(_, args) | TyKind::Dyn(_, args) => work.extend(args),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    while let Some(t) = work.pop() {
        if out.insert(t) {
            work.extend(parts(cx, t));
        }
    }
    out
}

/// The types `t` is built from.
fn parts(cx: &Ctx, t: TyId) -> Vec<TyId> {
    match cx.ty.kind(t) {
        TyKind::Adt(_, a) | TyKind::Dyn(_, a) | TyKind::Tuple(a) => a.clone(),
        TyKind::Array(x) | TyKind::Option(x) | TyKind::Shared(x) => vec![*x],
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => vec![*a, *b],
        TyKind::FnPtr {
            params,
            ret,
            throws,
        } => params.iter().copied().chain([*ret, *throws]).collect(),
        _ => vec![],
    }
}

/// Every function's effects, and the functions that may run through a function value or a
/// method call.
struct Program {
    fns: Vec<Option<Effects>>,
    /// Functions used as values anywhere.
    values: Vec<DefId>,
    /// Methods (functions with a `this`), by member name.
    methods: Vec<(String, DefId)>,
    /// Types the program uses as type arguments ([`type_args_closure`]).
    type_args: HashSet<TyId>,
    /// Types shared values hold inline ([`close_shared`]).
    shared: HashSet<TyId>,
}

impl Program {
    /// Does lowering pin a holder of type `t`: a shared object type it can count?
    fn pinned(&self, cx: &Ctx, t: TyId) -> bool {
        self.shared.contains(&t)
            && !has_params(cx, t)
            && matches!(cx.ty.kind(t), TyKind::Adt(d, _) if adt_kind(cx, *d) == Some(AdtKind::Anon))
    }

    /// May running the right-hand side `rhs` (its direct effects) assign a value of one of
    /// `tys`, directly or in what it calls?
    fn may_assign(&self, cx: &mut Ctx, rhs: &Effects, tys: &[TyId]) -> bool {
        if tys.is_empty() {
            return false;
        }
        if rhs.anything {
            return self.hits(cx, rhs, &None, tys)
                || self
                    .fns
                    .iter()
                    .flatten()
                    .any(|fx| self.hits(cx, fx, &None, tys));
        }
        let mut seen: HashSet<(DefId, Option<Vec<TyId>>)> = HashSet::new();
        let mut work: Vec<(&Effects, Option<Vec<TyId>>)> = vec![(rhs, None)];
        let mut values = false;
        let mut names: HashSet<Option<String>> = HashSet::new();
        while let Some((fx, frame)) = work.pop() {
            if self.hits(cx, fx, &frame, tys) {
                return true;
            }
            let mut next: Vec<(DefId, Option<Vec<TyId>>)> = Vec::new();
            for (d, f) in &fx.calls {
                let f = match f {
                    Frame::Inherit => frame.clone(),
                    Frame::Known(args) => Some(match &frame {
                        Some(outer) => args.iter().map(|a| cx.subst(*a, outer)).collect(),
                        None => args.clone(),
                    }),
                    Frame::Unknown => None,
                };
                next.push((*d, f));
            }
            if fx.indirect && !values {
                values = true;
                next.extend(self.values.iter().map(|d| (*d, None)));
            }
            for m in &fx.methods {
                if names.contains(&None) || !names.insert(m.clone()) {
                    continue;
                }
                let of = self
                    .methods
                    .iter()
                    .filter(|(n, _)| m.as_ref().is_none_or(|m| m == n));
                next.extend(of.map(|(_, d)| (*d, None)));
            }
            for (d, f) in next {
                if seen.len() >= MAX_VISITS {
                    return true;
                }
                if let Some(Some(callee)) = self.fns.get(d.0 as usize) {
                    if seen.insert((d, f.clone())) {
                        work.push((callee, f));
                    }
                }
            }
        }
        false
    }

    /// Does `fx`, run with type arguments `frame` (`None`: not known), assign one of `tys`?
    fn hits(&self, cx: &mut Ctx, fx: &Effects, frame: &Option<Vec<TyId>>, tys: &[TyId]) -> bool {
        if tys.iter().any(|t| fx.assigns.contains(t)) {
            return true;
        }
        fx.generic.iter().any(|&g| {
            let g = match frame {
                Some(args) => cx.subst(g, args),
                None => g,
            };
            if has_params(cx, g) {
                tys.iter().any(|&t| self.instance(cx, g, t, 0))
            } else {
                tys.contains(&g)
            }
        })
    }

    /// Can `p`, a type mentioning type parameters, be `t` once instantiated with types the
    /// program uses as type arguments?
    fn instance(&self, cx: &mut Ctx, p: TyId, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return true;
        }
        if !has_params(cx, p) {
            return p == t;
        }
        match (cx.ty.kind(p).clone(), cx.ty.kind(t).clone()) {
            (TyKind::Param(_), _) => self.type_args.contains(&t),
            (TyKind::Adt(d, a), TyKind::Adt(e, b)) if d == e => self.pairs(cx, &a, &b, depth),
            (TyKind::Adt(d, a), TyKind::Adt(e, b))
                if adt_kind(cx, d) == Some(AdtKind::Anon)
                    && adt_kind(cx, e) == Some(AdtKind::Anon) =>
            {
                // The same shape: generic `{ a: T }` and the concrete `{ a: string }`.
                let (Some(DefInfo::Adt(x)), Some(DefInfo::Adt(y))) =
                    (cx.info.get(d.0 as usize), cx.info.get(e.0 as usize))
                else {
                    return false;
                };
                if x.fields.len() != y.fields.len()
                    || x.fields
                        .iter()
                        .zip(&y.fields)
                        .any(|(f, g)| f.name != g.name)
                {
                    return false;
                }
                let fields: Vec<(TyId, TyId)> = x
                    .fields
                    .iter()
                    .zip(&y.fields)
                    .map(|(f, g)| (f.ty, g.ty))
                    .collect();
                fields.into_iter().all(|(f, g)| {
                    let (f, g) = (cx.subst(f, &a), cx.subst(g, &b));
                    self.instance(cx, f, g, depth + 1)
                })
            }
            (TyKind::Option(x), TyKind::Option(y)) => self.instance(cx, x, y, depth + 1),
            (TyKind::Tuple(a), TyKind::Tuple(b)) => self.pairs(cx, &a, &b, depth),
            _ => false,
        }
    }

    fn pairs(&self, cx: &mut Ctx, a: &[TyId], b: &[TyId], depth: u32) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| self.instance(cx, *x, *y, depth + 1))
    }
}
/// The assignment `place op= value` as a [`Candidate`], when its target lies in an object
/// literal read as a field of another value and the right-hand side may run code.
fn candidate(
    cx: &Ctx,
    names: &[String],
    span: Span,
    place: &Expr,
    value: &Expr,
) -> Option<Candidate> {
    let E::Field { base, index, .. } = &place.kind else {
        return None;
    };
    let mut fx = Effects::default();
    let mut b = crate::hir::Block {
        stmts: vec![],
        value: Some(Box::new(value.clone())),
        span: value.span,
    };
    let none = HashSet::new();
    visit::exprs_mut(&mut b, &mut |x: &mut Expr| fx.note(cx, &none, x));
    if !fx.runs_code() {
        return None;
    }
    let holder = base.ty;
    let TyKind::Adt(d, _) = cx.ty.kind(holder) else {
        return None;
    };
    if adt_kind(cx, *d) != Some(AdtKind::Anon) || contains_itself(cx, *d) {
        return None;
    }
    let mut path = Vec::new();
    let h = peel(base, &mut path);
    let E::Field { base: parent, .. } = &h.kind else {
        return None;
    };
    // The holders above: their types, up to a class object (pinned by lowering).
    let mut e: &Expr = parent;
    loop {
        if is_class(cx, e.ty) {
            break;
        }
        let inner = peel(e, &mut path);
        match &inner.kind {
            E::Field { base, .. } => e = base,
            _ => break,
        }
    }
    Some(Candidate {
        span,
        value_span: value.span,
        value: fx,
        path,
        holder_text: text(cx, names, h),
        parent_text: text(cx, names, parent),
        target_text: format!("{}.{}", text(cx, names, h), field_name(cx, holder, *index)),
    })
}

/// `e` without the narrowing around it (`o.inner!`), recording the type of each layer.
fn peel<'e>(mut e: &'e Expr, types: &mut Vec<TyId>) -> &'e Expr {
    loop {
        types.push(e.ty);
        match &e.kind {
            E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => e = x,
            _ => return e,
        }
    }
}

fn adt_kind(cx: &Ctx, d: DefId) -> Option<AdtKind> {
    match cx.info.get(d.0 as usize)? {
        DefInfo::Adt(a) => Some(a.kind),
        _ => None,
    }
}

fn is_class(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if adt_kind(cx, *d) == Some(AdtKind::Class))
}

/// Does the object type `d` reach itself through values stored inline (always a counted box)?
fn contains_itself(cx: &Ctx, d: DefId) -> bool {
    let mut seen = HashSet::new();
    let mut work: Vec<TyId> = field_types(cx, d);
    while let Some(t) = work.pop() {
        if !seen.insert(t) {
            continue;
        }
        match cx.ty.kind(t) {
            TyKind::Adt(x, _) if *x == d => return true,
            TyKind::Adt(x, _) if adt_kind(cx, *x) != Some(AdtKind::Class) => {
                work.extend(field_types(cx, *x))
            }
            TyKind::Option(x) => work.push(*x),
            TyKind::Tuple(ts) => work.extend(ts.iter().copied()),
            _ => {}
        }
    }
    false
}

fn field_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Adt(a)) => a.fields.iter().map(|f| f.ty).collect(),
        Some(DefInfo::Enum(e)) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// The types shared values hold inline: a shared object literal type that is never changed in
/// place is copied field by field, sharing what its fields hold.
fn close_shared(cx: &mut Ctx, mut work: Vec<TyId>) -> HashSet<TyId> {
    let assigned = crate::assigned_fields::assigned_objects(cx);
    let mut out = HashSet::new();
    while let Some(t) = work.pop() {
        if !out.insert(t) {
            continue;
        }
        match cx.ty.kind(t).clone() {
            TyKind::Option(x) => work.push(x),
            TyKind::Tuple(ts) => work.extend(ts),
            TyKind::Adt(d, args) => {
                let kind = adt_kind(cx, d);
                if kind == Some(AdtKind::Class) || assigned.contains(&d) {
                    continue;
                }
                for f in field_types(cx, d) {
                    let f = cx.subst(f, &args);
                    work.push(f);
                }
            }
            _ => {}
        }
    }
    out
}

fn field_name(cx: &Ctx, t: TyId, index: u32) -> String {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => match cx.info.get(d.0 as usize) {
            Some(DefInfo::Adt(a)) => a
                .fields
                .get(index as usize)
                .map(|f| f.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// A place as written (`o.inner`), for the message.
fn text(cx: &Ctx, names: &[String], e: &Expr) -> String {
    match &e.kind {
        E::Local(l, _) => names.get(l.0 as usize).cloned().unwrap_or_default(),
        E::Field { base, index, .. } => {
            format!(
                "{}.{}",
                text(cx, names, base),
                field_name(cx, base.ty, *index)
            )
        }
        E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => {
            text(cx, names, x)
        }
        E::Index { base, .. } => format!("{}[…]", text(cx, names, base)),
        _ => "…".into(),
    }
}
