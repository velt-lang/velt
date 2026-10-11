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
//!   name (and the interface's implementations), and an `await` anything. Code that runs
//!   without a call in the HIR (class hooks glue calls, `[Symbol.dispose]()` on a drop,
//!   generator bodies at a resume, initializers) counts as called (`implicit` lists every such
//!   place). A variable of the function that is not a parameter, not boxed and captured by no
//!   closure is replaced only by an assignment in the right-hand side itself. A right-hand side
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

mod effects;
mod fix;
mod implicit;
mod program;

use implicit::Implicit;
use program::Program;

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
    /// A variable of the function, not a parameter, not boxed and captured by no closure: only
    /// the right-hand side itself can assign it.
    local: Option<LocalId>,
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
    /// Types of the values it hands glue that may format, serialise or deep-copy them, calling
    /// class hooks (`implicit` 1 and 2).
    glue: HashSet<TyId>,
    /// Types of the values it may drop, running `[Symbol.dispose]()` hooks (`implicit` 3).
    drops: HashSet<TyId>,
    /// It resumes a generator: any generator body may run (`implicit` 4).
    resumes: bool,
    /// Its own variables it assigns whole values to.
    locals: HashSet<LocalId>,
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

/// One function body's scan: its effects, and what [`check`] collects across the program.
struct Scan<'a, 'm> {
    cx: &'a Ctx<'m>,
    slots: &'a [Option<Box<[bool]>>],
    im: &'a Implicit,
    names: &'a [String],
    locals: &'a [LocalDef],
    /// The function's parameters (`this` too).
    params: &'a HashSet<LocalId>,
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
        self.fx.note(self.cx, self.slots, &self.scope, self.im, e);
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
    im: &'a Implicit,
    locals: &'a [LocalDef],
    scope: Scope,
    fx: Effects,
}

impl VisitMut for Rhs<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        self.scope.constant(self.cx, self.locals, s);
    }

    fn expr(&mut self, e: &mut Expr) {
        self.fx.note(self.cx, self.slots, &self.scope, self.im, e);
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
    let im = Implicit::new(cx);
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
        let own_params: HashSet<LocalId> = f.params.iter().map(|p| p.local).collect();
        let first = candidates.len();
        let mut scan = Scan {
            cx,
            slots: &slots,
            im: &im,
            names: &names,
            locals: &locals,
            params: &own_params,
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
        // Its variables are dropped at the end of their scopes.
        for l in &locals {
            if im.maybe(cx, l.ty) {
                fx.drops.insert(l.ty);
            }
        }
        f.body.locals = locals;
        if candidates[first..]
            .iter()
            .any(|c| c.path.iter().any(|l| l.local.is_some()))
        {
            // A variable a closure captures may be assigned by it.
            let mut captured: HashSet<LocalId> = HashSet::new();
            visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
                if let E::Closure(d) = e.kind {
                    if let Some(Some(Def::Fn(c))) = cx.defs.get(d.0 as usize) {
                        captured.extend(c.captures.iter().map(|c| c.outer));
                    }
                }
            });
            for c in &mut candidates[first..] {
                for l in &mut c.path {
                    if l.local.is_some_and(|x| captured.contains(&x)) {
                        l.local = None;
                    }
                }
            }
        }
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
    // Module constants' initializers (computed at each use) and object types' field
    // initializers (evaluated where a value is built): each a function of its own, in the slot
    // of the constant's or the type's `DefId` (`implicit` 5).
    for (i, d) in cx.info.iter().enumerate() {
        let inits: Vec<&Expr> = match d {
            DefInfo::Global(g) => g.init.iter().collect(),
            DefInfo::Adt(a) => a.fields.iter().filter_map(|f| f.default.as_ref()).collect(),
            _ => continue,
        };
        if inits.is_empty() {
            continue;
        }
        let mut rhs = Rhs {
            cx,
            slots: &slots,
            im: &im,
            locals: &[],
            scope: Scope::default(),
            fx: Effects::default(),
        };
        for x in inits {
            let mut x = x.clone();
            visit::expr(&mut x, &mut Values(&mut values, &mut type_args));
            visit::expr(&mut x, &mut rhs);
        }
        let mut fx = rhs.fx;
        type_args.append(&mut fx.type_args);
        if fns[i].is_none() && fx.runs_code() {
            fns[i] = Some(fx);
        }
    }
    // A resume may run any generator body: each is a function value (`implicit` 4).
    values.extend(im.generators.iter().copied());
    // A function used as a value runs with whatever its caller passes.
    for d in &values {
        if let Some(Some(fx)) = fns.get_mut(d.0 as usize) {
            fx.indirect |= fx.own;
        }
    }
    let shared = program::close_shared(cx, shared);
    let program = Program {
        fns,
        values,
        methods,
        type_args: (type_args, std::cell::OnceCell::new()),
        shared,
        im,
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
        im: scan.im,
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
        let start = path.len();
        let inner = peel(cx, e, &mut path);
        match &inner.kind {
            E::Field { base, .. } => e = base,
            &E::Local(l, _) => {
                // Another function reaches a variable only through a capture.
                let own = !scan.params.contains(&l)
                    && scan.locals.get(l.0 as usize).is_some_and(|d| !d.boxed);
                if own {
                    for x in &mut path[start..] {
                        x.local = Some(l);
                    }
                }
                break;
            }
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
            local: None,
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
