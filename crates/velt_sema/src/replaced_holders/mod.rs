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
//! - the right-hand side may replace the target's object or a holder above it (up to the
//!   nearest class object or shared object, which lowering pins), directly or in a function it
//!   calls (transitively): assign the field holding it (that field of that object type: of a
//!   readonly view's plain twin, of the base class declaring it; another object type holding
//!   the same object converts by copying), or a whole value of its type to a variable or an
//!   element where it is not held in a field. A generic function's field of a type-parameter
//!   type is substituted with the call's type arguments; where those are not known, it may be
//!   any type the program uses as a type argument. A call through a function value may run any
//!   function the program uses as a value, except that a function a call's argument or a
//!   constant names is followed instead, and a function calling its own parameter (of a
//!   function type, or one `| null`, never reassigned) runs what its callers passed. That holds
//!   only for a function never used as a value, so every caller writes the arguments: one
//!   naming a function is followed, the caller's own parameter and `null` are fine, and
//!   anything else in such a parameter (of any type: a type parameter's value may hold a
//!   function) may run any function value. A function used as a value is called by code that
//!   passes anything (`fs.map((f) => f())`), so its calls of its parameters may run any
//!   function value. A method call through a vtable or an interface runs the methods of that
//!   name (and the interface's implementations), and an `await` anything. A right-hand side
//!   that only reads, computes and builds values compiles.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{
    AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, Lit, LocalDef, LocalId, Stmt,
    StmtKind as S, TyId, TyKind,
};
use crate::visit::{self, VisitMut};

mod fix;

/// Most (function, type arguments) pairs [`Program::may_assign`] visits before it gives up and
/// answers "may".
const MAX_VISITS: usize = 20_000;

/// A field a whole value is assigned to: the object type declaring it (a readonly view's plain
/// twin, a subclass's base class that declares the field) and its name (hashed; a collision
/// only makes the check stricter). Only an assignment to that field of that type replaces the
/// object stored there: another type holding the same object converts by copying
/// (`crate::object_copies`).
type FieldKey = (DefId, u64);

/// A field of a generic object type (whose instances are other types), by name (hashed) and
/// field type.
type LooseKey = (u64, TyId);

/// Where the object a target writes into, or a holder above it, is stored.
#[derive(Clone, Copy, Debug)]
struct Layer {
    ty: TyId,
    /// The field holding it (`None`: a variable, an element, a computed value or a field of a
    /// generic object type, replaced by a whole value of its type assigned anywhere).
    field: Option<(FieldKey, LooseKey)>,
    /// `ty` mentions type parameters (in a generic function): any instance counts.
    generic: bool,
}

/// One assignment whose target lies in an inline object literal.
struct Candidate {
    span: Span,
    value_span: Span,
    /// What the right-hand side runs.
    value: Effects,
    /// The places whose assignment replaces the object the target lies in: its own first, then
    /// its holders', up to a class object (pinned by lowering).
    path: Vec<Layer>,
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
    /// The fields among them, of an object type without type parameters.
    fields: HashSet<FieldKey>,
    /// The others (of a generic object type, with a field type without).
    loose: HashSet<LooseKey>,
    /// Types of the fields it assigns whose type mentions a type parameter.
    generic: Vec<TyId>,
    /// Functions it calls directly (`new` calls the constructor, an interface call each
    /// implementation), the callbacks written in the arguments of its calls and the functions
    /// its constants hold, called or passed.
    calls: Vec<(DefId, Frame)>,
    /// It calls a function value other than those, or hands a call one: any function the
    /// program uses as a value may run.
    indirect: bool,
    /// It calls or hands on its own function parameters. They run what its callers passed when
    /// every caller is a direct call, which names them in its arguments; a function used as a
    /// value is called by code that passes anything (`fs.map((f) => f())`), so there this
    /// counts as [`Effects::indirect`].
    own: bool,
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

/// The function values a body can name: its own parameters taking one (never reassigned) and
/// its constants holding a closure or a function.
#[derive(Default)]
struct Scope {
    params: HashSet<LocalId>,
    known: HashMap<LocalId, (DefId, Frame)>,
}

/// What a function value expression holds.
enum FnValue {
    /// The function's own parameter (or nothing: `null`).
    Own,
    /// This function.
    Known(DefId, Frame),
    /// Any function value.
    Other,
}

impl Scope {
    /// A constant holding a function: calling it runs that function.
    fn constant(&mut self, cx: &Ctx, locals: &[LocalDef], s: &Stmt) {
        if let S::Let {
            local,
            init: Some(init),
        } = &s.kind
        {
            let fixed = locals
                .get(local.0 as usize)
                .is_some_and(|l| !l.mutable && !l.boxed);
            if let (true, FnValue::Known(d, f)) = (fixed, Scope::default().classify(cx, init)) {
                self.known.insert(*local, (d, f));
            }
        }
    }

    fn classify(&self, cx: &Ctx, x: &Expr) -> FnValue {
        match &x.kind {
            E::Lit(Lit::Null) => FnValue::Own,
            E::WrapSome(x) | E::UnwrapSome(x, _) | E::Upcast(x) => self.classify(cx, x),
            // A module-level constant: its value, computed at each use.
            E::Global(d) => match cx.info.get(d.0 as usize) {
                Some(DefInfo::Global(g)) if g.init.is_some() => {
                    let Some(mut e) = g.init.as_ref() else {
                        return FnValue::Other;
                    };
                    while let E::WrapSome(x) | E::Upcast(x) = &e.kind {
                        e = x;
                    }
                    match e.kind {
                        E::Closure(_) | E::FnRef(..) | E::Lit(Lit::Null) => {
                            Scope::default().classify(cx, e)
                        }
                        _ => FnValue::Other,
                    }
                }
                _ => FnValue::Other,
            },
            E::Closure(d) => FnValue::Known(*d, Frame::Inherit),
            E::FnRef(d, targs) => FnValue::Known(*d, Frame::Known(targs.clone())),
            E::Local(l, _) if self.params.contains(l) => FnValue::Own,
            E::Local(l, _) => match self.known.get(l) {
                Some((d, f)) => FnValue::Known(*d, f.clone()),
                None => FnValue::Other,
            },
            _ => FnValue::Other,
        }
    }
}

/// Is `t` a function value?
fn is_fn(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::FnPtr { .. } | TyKind::Closure(_))
}

/// Is `t` a function value or an optional one (`((x: T) => U) | null`)?
fn fn_or_null(cx: &Ctx, t: TyId) -> bool {
    is_fn(cx, t) || matches!(cx.ty.kind(t), TyKind::Option(x) if is_fn(cx, *x))
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

fn name_hash(cx: &Ctx, parent: TyId, index: u32) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    field_name(cx, parent, index).hash(&mut h);
    h.finish()
}

/// The keys of field `index` of `parent`, of type `ty` (`None` for a parent that is not an
/// object type).
fn field_keys(cx: &Ctx, parent: TyId, index: u32, ty: TyId) -> Option<(FieldKey, LooseKey)> {
    let &TyKind::Adt(mut d, _) = cx.ty.kind(parent) else {
        return None;
    };
    let name = name_hash(cx, parent, index);
    loop {
        if let Some(&(twin, _)) = cx.readonly_twins.get(&d) {
            if twin != d {
                d = twin;
                continue;
            }
        }
        // A class's inherited field is its base's (the base's fields come first).
        let base = match cx.info.get(d.0 as usize) {
            Some(DefInfo::Adt(a)) => a.base.and_then(|b| match cx.ty.kind(b) {
                &TyKind::Adt(b, _) => match cx.info.get(b.0 as usize) {
                    Some(DefInfo::Adt(x)) if (index as usize) < x.fields.len() => Some(b),
                    _ => None,
                },
                _ => None,
            }),
            _ => None,
        };
        match base {
            Some(b) => d = b,
            None => return Some(((d, name), (name, ty))),
        }
    }
}

/// Which parameters of each function (by [`DefId`]) take a function value or `null`: a call
/// passing an argument there that names no function (a type parameter's value can hold one)
/// may run any function value.
fn fn_slots(cx: &Ctx) -> Vec<Option<Box<[bool]>>> {
    cx.defs
        .iter()
        .map(|d| match d {
            Some(Def::Fn(f)) if f.params.iter().any(|p| fn_or_null(cx, p.ty)) => {
                Some(f.params.iter().map(|p| fn_or_null(cx, p.ty)).collect())
            }
            _ => None,
        })
        .collect()
}

fn slot_of(slots: &[Option<Box<[bool]>>], d: DefId) -> Option<&[bool]> {
    slots.get(d.0 as usize).and_then(|s| s.as_deref())
}

impl Effects {
    fn runs_code(&self) -> bool {
        !self.assigns.is_empty()
            || !self.generic.is_empty()
            || !self.calls.is_empty()
            || self.indirect
            || self.own
            || !self.methods.is_empty()
            || self.anything
    }

    fn note(&mut self, cx: &Ctx, slots: &[Option<Box<[bool]>>], scope: &Scope, e: &Expr) {
        match &e.kind {
            // Only a field holds an object literal inline (an element replaced is #820).
            E::Assign { place, .. } => match &place.kind {
                E::Field { .. } if has_params(cx, place.ty) => self.generic.push(place.ty),
                E::Field { base, index, .. } => {
                    self.assigns.insert(place.ty);
                    match field_keys(cx, base.ty, *index, place.ty) {
                        Some((_, loose)) if has_params(cx, base.ty) => {
                            self.loose.insert(loose);
                        }
                        Some((key, _)) => {
                            self.fields.insert(key);
                        }
                        None => {}
                    }
                }
                _ => {
                    self.assigns.insert(place.ty);
                }
            },
            E::Call { callee, args } => match callee {
                Callee::Def(d, targs) => {
                    self.callbacks(cx, scope, args, slot_of(slots, *d));
                    self.calls.push((*d, Frame::Known(targs.clone())));
                    self.type_args.extend(targs);
                }
                Callee::Intrinsic(_) => self.callbacks(cx, scope, args, None),
                Callee::Indirect(f) => {
                    self.callbacks(cx, scope, args, None);
                    self.run(scope.classify(cx, f));
                }
                Callee::Virtual { slot } => {
                    self.callbacks(cx, scope, args, None);
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
                    self.callbacks(cx, scope, args, None);
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
                    self.callbacks(cx, scope, args, None);
                    self.type_args.extend(iface_args);
                    self.type_args.extend(method_type_args);
                    self.iface_call(cx, Some(*iface), *slot);
                }
            },
            E::New {
                def,
                type_args,
                args,
            } => {
                self.type_args.extend(type_args);
                match cx.info.get(def.0 as usize) {
                    Some(DefInfo::Adt(a)) => {
                        let ctor = a.ctor;
                        let s = ctor.and_then(|c| slot_of(slots, c));
                        self.callbacks(cx, scope, args, s);
                        self.calls
                            .extend(ctor.map(|c| (c, Frame::Known(type_args.clone()))));
                    }
                    _ => {
                        self.callbacks(cx, scope, args, None);
                        self.anything = true;
                    }
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

    /// Running (or handing on) the function value `v`.
    fn run(&mut self, v: FnValue) {
        match v {
            FnValue::Own => self.own = true,
            FnValue::Known(d, f) => self.calls.push((d, f)),
            FnValue::Other => self.indirect = true,
        }
    }

    /// The function values among a call's arguments: a function an argument names runs (it is
    /// followed), the function's own parameter is its callers' business, and anything else
    /// may be any function value. `slots`: which of the callee's parameters take a function
    /// value (aligned with the arguments at the end, past a receiver passed apart); there an
    /// argument of any type counts.
    fn callbacks(&mut self, cx: &Ctx, scope: &Scope, args: &[Expr], slots: Option<&[bool]>) {
        let skip = slots.map_or(0, |s| s.len() as isize - args.len() as isize);
        for (i, a) in args.iter().enumerate() {
            let slot = slots.is_some_and(|s| {
                skip < 0 || s.get((i as isize + skip) as usize).copied().unwrap_or(true)
            });
            if slot || fn_or_null(cx, a.ty) {
                self.run(scope.classify(cx, a));
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

/// One function body's scan: its effects, and what [`check`] collects across the program.
struct Scan<'a, 'm> {
    cx: &'a Ctx<'m>,
    slots: &'a [Option<Box<[bool]>>],
    names: &'a [String],
    locals: &'a [LocalDef],
    scope: Scope,
    fx: Effects,
    shared: &'a mut Vec<TyId>,
    candidates: &'a mut Vec<Candidate>,
}

impl VisitMut for Scan<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        self.scope.constant(self.cx, self.locals, s);
    }

    fn expr(&mut self, e: &mut Expr) {
        self.fx.note(self.cx, self.slots, &self.scope, e);
        match &e.kind {
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                ..
            } => self.shared.push(e.ty),
            E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
                let c = candidate(self, e.span, place, value);
                self.candidates.extend(c);
            }
            _ => {}
        }
    }
}

/// A right-hand side's scan: what it runs.
struct Rhs<'a, 'm> {
    cx: &'a Ctx<'m>,
    slots: &'a [Option<Box<[bool]>>],
    locals: &'a [LocalDef],
    scope: Scope,
    fx: Effects,
}

impl VisitMut for Rhs<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        self.scope.constant(self.cx, self.locals, s);
    }

    fn expr(&mut self, e: &mut Expr) {
        self.fx.note(self.cx, self.slots, &self.scope, e);
    }
}

/// Does the program assign into an object literal read as a field of another (the shape of a
/// [`Candidate`])? Most programs do not, and then nothing else needs to be looked at.
fn any_candidate(cx: &mut Ctx) -> bool {
    let mut found = false;
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                if let E::Field { base, .. } = &place.kind {
                    found |= matches!(cx.ty.kind(base.ty), &TyKind::Adt(d, _)
                        if adt_kind(cx, d) == Some(AdtKind::Anon))
                        && matches!(unnarrowed(base).kind, E::Field { .. });
                }
            }
        });
        cx.defs[i] = Some(Def::Fn(f));
        if found {
            return true;
        }
    }
    false
}

/// Collects the functions an expression uses as values, and the type arguments it passes.
struct Values<'a>(&'a mut Vec<DefId>, &'a mut Vec<TyId>);

impl VisitMut for Values<'_> {
    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Closure(d) => self.0.push(*d),
            E::FnRef(d, targs) => {
                self.0.push(*d);
                self.1.extend(targs);
            }
            E::Call {
                callee: Callee::Def(_, targs),
                ..
            } => self.1.extend(targs),
            _ => {}
        }
    }
}

/// Report the assignments the module docs describe.
pub(crate) fn check(cx: &mut Ctx) {
    if !any_candidate(cx) {
        return;
    }
    let slots = fn_slots(cx);
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
        let mut params: HashSet<LocalId> = HashSet::new();
        if slots[i].is_some() {
            params = f
                .params
                .iter()
                .filter(|p| fn_or_null(cx, p.ty))
                .filter(|p| {
                    f.body
                        .locals
                        .get(p.local.0 as usize)
                        .is_some_and(|l| !l.boxed)
                })
                .map(|p| p.local)
                .collect();
            // Never reassigned (`mutable` also marks a parameter whose closure's state changes).
            visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
                if let E::Assign { place, .. } | E::CompoundAssign { place, .. } = &e.kind {
                    if let E::Local(l, _) = place.kind {
                        params.remove(&l);
                    }
                }
            });
        }
        let locals = std::mem::take(&mut f.body.locals);
        let mut scan = Scan {
            cx,
            slots: &slots,
            names: &names,
            locals: &locals,
            scope: Scope {
                params,
                known: HashMap::new(),
            },
            fx: Effects::default(),
            shared: &mut shared,
            candidates: &mut candidates,
        };
        visit::block(&mut f.body.block, &mut scan);
        let mut fx = scan.fx;
        f.body.locals = locals;
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
    // Functions module-level constants hold (their value is computed at each use).
    for d in &cx.info {
        if let DefInfo::Global(g) = d {
            if let Some(init) = &g.init {
                visit::expr(&mut init.clone(), &mut Values(&mut values, &mut type_args));
            }
        }
    }
    // A function used as a value runs with whatever its caller passes.
    for d in &values {
        if let Some(Some(fx)) = fns.get_mut(d.0 as usize) {
            fx.indirect |= fx.own;
        }
    }
    let shared = close_shared(cx, shared);
    let program = Program {
        fns,
        values,
        methods,
        type_args: (type_args, std::cell::OnceCell::new()),
        shared,
    };
    let mut counted = Vec::new();
    for c in candidates {
        // The holders lowering does not pin: up to the nearest shared object.
        let open = c
            .path
            .iter()
            .take_while(|l| !program.pinned(cx, l.ty))
            .count();
        if program.may_assign(cx, &c.value, &c.path[..open]) {
            fix::report(cx, &c);
        } else if open < c.path.len() && program.may_assign(cx, &c.value, &c.path) {
            // Accepted because a holder is shared: lowering must count it.
            counted.push(c.path[open].ty);
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
    /// The type arguments the program passes, and every type nested in them or in those of the
    /// types it uses ([`type_args_closure`], computed when first needed).
    type_args: (Vec<TyId>, std::cell::OnceCell<HashSet<TyId>>),
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

    /// May running the right-hand side `rhs` (its direct effects) replace one of `path`'s
    /// objects, directly or in what it calls?
    fn may_assign(&self, cx: &mut Ctx, rhs: &Effects, tys: &[Layer]) -> bool {
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

    /// Does `fx`, run with type arguments `frame` (`None`: not known), replace one of `path`'s
    /// objects: assign its field (a whole value of its type, for one not held in a field), or a
    /// generic field that may be it?
    fn hits(&self, cx: &mut Ctx, fx: &Effects, frame: &Option<Vec<TyId>>, path: &[Layer]) -> bool {
        let direct = |l: &Layer| match l.field {
            Some((key, loose)) => fx.fields.contains(&key) || fx.loose.contains(&loose),
            None => fx.assigns.contains(&l.ty),
        };
        if path.iter().any(direct) {
            return true;
        }
        for l in path.iter().filter(|l| l.generic) {
            let assigns: Vec<TyId> = fx.assigns.iter().copied().collect();
            if assigns.into_iter().any(|a| self.instance(cx, l.ty, a, 0)) {
                return true;
            }
        }
        fx.generic.iter().any(|&g| {
            let g = match frame {
                Some(args) => cx.subst(g, args),
                None => g,
            };
            if has_params(cx, g) {
                path.iter().any(|l| self.instance(cx, g, l.ty, 0))
            } else {
                path.iter().any(|l| l.ty == g)
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
            (TyKind::Param(_), _) => {
                let (passed, all) = &self.type_args;
                all.get_or_init(|| type_args_closure(cx, passed.clone()))
                    .contains(&t)
            }
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
fn candidate(scan: &mut Scan<'_, '_>, span: Span, place: &Expr, value: &Expr) -> Option<Candidate> {
    let cx = scan.cx;
    let E::Field { base, index, .. } = &place.kind else {
        return None;
    };
    let holder = base.ty;
    let &TyKind::Adt(d, _) = cx.ty.kind(holder) else {
        return None;
    };
    if adt_kind(cx, d) != Some(AdtKind::Anon) {
        return None;
    }
    let mut path = Vec::new();
    let h = peel(cx, base, &mut path);
    let E::Field { base: parent, .. } = &h.kind else {
        return None;
    };
    // What the right-hand side runs: it names the function's constants, not its parameters
    // (their callers are not known yet).
    let mut rhs = Rhs {
        cx,
        slots: scan.slots,
        locals: scan.locals,
        scope: Scope {
            params: HashSet::new(),
            known: std::mem::take(&mut scan.scope.known),
        },
        fx: Effects::default(),
    };
    visit::expr(&mut value.clone(), &mut rhs);
    let fx = rhs.fx;
    scan.scope.known = rhs.scope.known;
    if !fx.runs_code() || contains_itself(cx, d) {
        return None;
    }
    // The holders above, up to a class object (pinned by lowering).
    let mut e: &Expr = parent;
    loop {
        if is_class(cx, e.ty) {
            break;
        }
        let inner = peel(cx, e, &mut path);
        match &inner.kind {
            E::Field { base, .. } => e = base,
            _ => break,
        }
    }
    let names = scan.names;
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

/// `e` without the narrowing around it (`o.inner!`).
fn unnarrowed(mut e: &Expr) -> &Expr {
    while let E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } = &e.kind {
        e = x;
    }
    e
}

/// `e` without the narrowing around it (`o.inner!`), recording a layer for each type it is
/// seen as, all in the field `e` reads (if it reads one).
fn peel<'e>(cx: &Ctx, mut e: &'e Expr, path: &mut Vec<Layer>) -> &'e Expr {
    let start = path.len();
    loop {
        path.push(Layer {
            ty: e.ty,
            field: None,
            generic: has_params(cx, e.ty),
        });
        match &e.kind {
            E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => e = x,
            _ => break,
        }
    }
    if let E::Field { base, index, .. } = &e.kind {
        let k = (!has_params(cx, base.ty))
            .then(|| field_keys(cx, base.ty, *index, e.ty))
            .flatten();
        for l in &mut path[start..] {
            l.field = k;
        }
    }
    e
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
