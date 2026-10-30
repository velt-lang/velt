//! User code that runs without a call in the HIR: what lowering, its glue and the runtime call
//! on their own. The analysis counts each as a call, so a right-hand side reaching one is
//! checked like one that calls the code itself.
//!
//! The complete list (every place lowering or the runtime runs a function body that no HIR
//! `Callee` names; a new one must be added here):
//!
//! 1. **Class hooks called by formatting and serialising glue** (`hir::AdtDef::to_string`,
//!    `to_json`, `inspect`): `String(x)`, template literals, `+` on a string and `join` /
//!    `toString` of arrays (std code over a `T`) call `toString()`; `JSON.stringify` calls
//!    `toJSON()`; `console.log` / `console.error` call `__inspect()`, all for the value and for
//!    every value it holds. Counted for the arguments of the intrinsics in [`glue_args`] whose
//!    type reaches a class with such a method ([`Implicit::reach`]).
//! 2. **A class's own `clone()`**, which a deep copy calls instead of copying a resource owner
//!    field by field (velt_vir transfer.rs `own_clone`): `x.clone()` (`Intrinsic::Clone`), a
//!    transfer to another task (`Transfer`, `spawn`, channels, `shared`, `Mutex`, promise
//!    combinators, the arguments of an async call): for `spawn`, the arguments and receiver of
//!    the call it starts, whatever the callee (a function value, a vtable or interface method:
//!    velt_vir `async_fn/spawn.rs` `transfer_call`), the captures of an async closure, and
//!    either branch of a conditional. Counted like the hooks of 1.
//! 3. **`[Symbol.dispose]()`, run by drop glue** (`hir::AdtDef::dispose`): wherever a value is
//!    dropped (the end of its variable's scope, a temporary, the old value an assignment
//!    replaces, an element an array operation removes, a closure's environment, a `using`
//!    declaration). Counted for every local of a function and every value an expression
//!    computes (not a place it reads) whose type reaches a class with that method; a generic
//!    type by the type arguments of the call, a function value by what closures capture.
//! 4. **Generator bodies, run by a resume** (`Intrinsic::GeneratorResume`, `GeneratorReturn`,
//!    their async forms; `next()`, `return()`, `for...of`, spread and `Array.from` reach them
//!    through std's `Generator`), and by a generator's drop (its `[Symbol.dispose]()` runs the
//!    body's `finally` blocks, 3). A resume may run the body of any generator function in the
//!    program, so each counts as a function value too (its calls of its own parameters run
//!    anything its callers could pass).
//! 5. **Initializers evaluated where they are used**: a module constant's initializer at each
//!    read (`E::Global`), and field initializers by `new` / an object literal of a class that
//!    has no constructor of its own to run them. Each is scanned as a function of its own
//!    (stored in the slot of the constant's or the type's `DefId`).
//! 6. **Function values called by glue or the runtime** (thunks of named functions, promise
//!    reactions, timers, `http` handlers, `Mutex.with`, `attempt`, sort comparators,
//!    `Map`/`Set` callbacks, iterator helpers): always a function value some code passed, so
//!    already counted where it is passed or created (`Effects::callbacks`, `Effects::values`).
//! 7. **Async bodies after a suspension**: only at an `await` (`Effects::anything`).
//!
//! 8. **Releasing a weakly held object** (velt_rt weak tables, docs/internals/design/weak-refs.md):
//!    when a `WeakMap` key's last reference goes, the runtime removes its entries from every map
//!    and drops their values (their `[Symbol.dispose]()`, 3); dropping a map (std's
//!    `WeakMap[Symbol.dispose]`, an extern call) drops all its values. So a value of a key type
//!    reaches what the values of the maps keyed by it reach, and a `WeakMap<K, V>` what a `V`
//!    does. The maps are found by their `new WeakMap<K, V>()` ([`weak_entries`]); a key type that
//!    is a type parameter makes every object type a possible key. `WeakSet` and `WeakRef` own
//!    nothing they could drop.
//!
//! Lowering also calls std methods from glue (`Map` methods for equality and hashing of
//! dictionaries, `Record.__positions` / `__decoded` / `set` for JSON): std code that runs no
//! user code. Equality, hashing, sharing and truthiness are generated and call nothing.

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::collections::BTreeSet;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};

/// The methods glue calls on a class (1 and 2).
const GLUE_HOOKS: [&str; 4] = ["toString", "toJSON", "__inspect", "clone"];

/// What dropping, formatting or copying a value of some type may reach.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Reach {
    /// Classes with hooks (and their relatives, which a value of the type may be).
    adts: BTreeSet<DefId>,
    /// A type parameter: whatever the type arguments the program passes reach.
    param: bool,
    /// A function value: whatever a closure's captured values reach.
    fns: bool,
}

impl Reach {
    fn add(&mut self, o: &Reach) {
        self.adts.extend(o.adts.iter().copied());
        self.param |= o.param;
        self.fns |= o.fns;
    }
}

/// The implicit calls of the program: the hooks of each class, its generator functions and the
/// initializers that run code.
pub(crate) struct Implicit {
    /// Each class with a hook, and the hooks of its family (bases and subclasses, which a value
    /// of it may be or inherit from): `[Symbol.dispose]()`, and the glue hooks.
    hooks: FxHashMap<DefId, (Vec<DefId>, Vec<DefId>)>,
    /// Each class's subclasses (transitively).
    family: FxHashMap<DefId, Vec<DefId>>,
    /// Every generator function (4).
    pub(crate) generators: Vec<DefId>,
    /// The types of the values closures capture.
    captures: Vec<TyId>,
    /// Some closure captures a value whose type may reach a hook.
    captures_reach: bool,
    /// The prelude's `WeakMap` class.
    weak_map: Option<DefId>,
    /// The key and value types of the program's weak maps (8): a key `None` is a type
    /// parameter (any object may be a key).
    weak_entries: Vec<(Option<TyId>, TyId)>,
    /// [`Reach`] of each type seen without type arguments known (types mentioning type
    /// parameters keep them as [`Reach::param`]).
    memo: RefCell<FxHashMap<TyId, std::rc::Rc<Reach>>>,
    /// The lowest position on the walk's stack a walk below the current object type found its
    /// way back to (`usize::MAX`: none): a type whose walk returned to no type above it is
    /// complete, and its [`Reach`] goes into `memo` (#859: types sharing parts are walked once).
    cut: std::cell::Cell<usize>,
}

impl Implicit {
    /// `weak_entries`: [`weak_entries`] of the program.
    pub(crate) fn new(cx: &Ctx, weak_entries: Vec<(Option<TyId>, TyId)>) -> Implicit {
        let mut own: FxHashMap<DefId, (Vec<DefId>, Vec<DefId>)> = FxHashMap::default();
        let mut bases: Vec<(DefId, DefId)> = Vec::new();
        for (i, info) in cx.info.iter().enumerate() {
            let DefInfo::Adt(a) = info else { continue };
            let d = DefId(i as u32);
            let get = |n: &str| a.methods.get(n).filter(|m| !m.is_static).map(|m| m.def);
            let dispose: Vec<DefId> = get(crate::collect::DISPOSE).into_iter().collect();
            let glue: Vec<DefId> = GLUE_HOOKS.iter().filter_map(|n| get(n)).collect();
            if !dispose.is_empty() || !glue.is_empty() {
                own.insert(d, (dispose, glue));
            }
            if let Some(&TyKind::Adt(b, _)) = a.base.map(|b| cx.ty.kind(b)) {
                bases.push((d, b));
            }
        }
        // Family: a class's bases and its subclasses, transitively.
        let parent: FxHashMap<DefId, DefId> = bases.iter().copied().collect();
        let mut children: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
        for &(d, b) in &bases {
            children.entry(b).or_default().push(d);
        }
        let family: FxHashMap<DefId, Vec<DefId>> = parent
            .keys()
            .chain(children.keys())
            .map(|&d| {
                let mut v = vec![d];
                let mut x = d;
                while let Some(&b) = parent.get(&x) {
                    if v.contains(&b) {
                        break;
                    }
                    v.push(b);
                    x = b;
                }
                let mut work = vec![d];
                while let Some(x) = work.pop() {
                    for &c in children.get(&x).into_iter().flatten() {
                        if !v.contains(&c) {
                            v.push(c);
                            work.push(c);
                        }
                    }
                }
                (d, v)
            })
            .collect();
        let mut hooks: FxHashMap<DefId, (Vec<DefId>, Vec<DefId>)> = FxHashMap::default();
        for (&d, h) in &own {
            for &m in family.get(&d).map_or(&[d][..], |v| &v[..]) {
                let e = hooks.entry(m).or_default();
                e.0.extend(&h.0);
                e.1.extend(&h.1);
            }
        }
        let mut generators = Vec::new();
        let mut captures = Vec::new();
        for (i, def) in cx.defs.iter().enumerate() {
            let Some(Def::Fn(f)) = def else { continue };
            if f.is_generator {
                generators.push(DefId(i as u32));
            }
            for c in &f.captures {
                if let Some(l) = f.body.locals.get(c.inner.0 as usize) {
                    captures.push(l.ty);
                }
            }
        }
        captures.sort();
        captures.dedup();
        let weak_map = cx.prelude_adt("WeakMap");
        let mut im = Implicit {
            hooks,
            family,
            generators,
            captures,
            weak_map,
            weak_entries,
            captures_reach: false,
            memo: RefCell::new(FxHashMap::default()),
            cut: std::cell::Cell::new(usize::MAX),
        };
        im.captures_reach = im.captures.iter().any(|&t| {
            let r = im.reach(cx, t);
            !r.adts.is_empty() || r.param
        });
        im
    }

    /// May dropping, formatting or copying a value of type `t` run user code?
    pub(super) fn maybe(&self, cx: &Ctx, t: TyId) -> bool {
        let r = self.reach(cx, t);
        !r.adts.is_empty() || r.param || (r.fns && self.captures_reach)
    }

    /// What a value of type `t` reaches (memoized).
    pub(super) fn reach(&self, cx: &Ctx, t: TyId) -> std::rc::Rc<Reach> {
        if let Some(r) = self.memo.borrow().get(&t) {
            return r.clone();
        }
        let mut r = Reach::default();
        self.reach_in(cx, t, None, &mut Vec::new(), &mut r);
        let r = std::rc::Rc::new(r);
        self.memo.borrow_mut().insert(t, r.clone());
        r
    }

    /// Adds what `t` reaches to `out`; `env`: what the type parameters stand for (`None`: not
    /// known, [`Reach::param`]); `stack`: the object types being walked (a type reached again
    /// adds nothing new).
    fn reach_in(
        &self,
        cx: &Ctx,
        t: TyId,
        env: Option<&[Reach]>,
        stack: &mut Vec<DefId>,
        out: &mut Reach,
    ) {
        match cx.ty.kind(t) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error
            | TyKind::Symbol
            | TyKind::Simd(_)
            | TyKind::Literal(_) => {}
            TyKind::Param(i) => match env.and_then(|e| e.get(*i as usize)) {
                Some(r) => out.add(r),
                None => out.param = true,
            },
            TyKind::FnPtr { .. } | TyKind::Closure(_) => out.fns = true,
            TyKind::Array(x) => {
                let x = *x;
                self.weak_values(cx, t, None, stack, out);
                self.reach_in(cx, x, env, stack, out)
            }
            TyKind::Option(x, _) | TyKind::Shared(x) => self.reach_in(cx, *x, env, stack, out),
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                let (a, b) = (*a, *b);
                self.reach_in(cx, a, env, stack, out);
                self.reach_in(cx, b, env, stack, out);
            }
            TyKind::Tuple(ts) => {
                for &x in ts {
                    self.reach_in(cx, x, env, stack, out);
                }
            }
            // A deferred type is made of its arguments, and may hold functions of them.
            TyKind::Op(_, args) => {
                for &x in args {
                    self.reach_in(cx, x, env, stack, out);
                }
                out.fns = true;
            }
            TyKind::Adt(d, args) => {
                let (d, args) = (*d, args.clone());
                if let Some(at) = stack.iter().position(|x| *x == d) {
                    self.cut.set(self.cut.get().min(at));
                    // The same definition with other type arguments (`Box<Hooked>` inside
                    // `Box<number>`): the walk on the stack covers its fields, not what its
                    // arguments reach.
                    for &a in &args {
                        self.reach_in(cx, a, env, stack, out);
                    }
                    return;
                }
                // A type without type parameters reaches the same whatever `env` is.
                let concrete = !super::has_params(cx, t);
                if concrete {
                    if let Some(r) = self.memo.borrow().get(&t) {
                        out.add(r);
                        return;
                    }
                }
                let depth = stack.len();
                let outer = self.cut.replace(usize::MAX);
                let mut mine = Reach::default();
                self.adt_reach(cx, (t, d, &args), env, stack, &mut mine);
                let cut = self.cut.get();
                self.cut.set(outer.min(cut));
                if concrete && cut >= depth {
                    self.memo
                        .borrow_mut()
                        .insert(t, std::rc::Rc::new(mine.clone()));
                }
                out.add(&mine);
            }
            TyKind::Dyn(i, _) => {
                let i = *i;
                for imp in cx.impls.iter().filter(|x| x.iface == i) {
                    self.reach_in(cx, imp.ty, None, stack, out);
                }
            }
        }
    }

    /// [`Self::reach_in`] of the object type `t` (`d` with `args`), not on the stack.
    fn adt_reach(
        &self,
        cx: &Ctx,
        (t, d, args): (TyId, DefId, &[TyId]),
        env: Option<&[Reach]>,
        stack: &mut Vec<DefId>,
        out: &mut Reach,
    ) {
        let argr: Vec<Reach> = args
            .iter()
            .map(|&a| {
                let mut r = Reach::default();
                self.reach_in(cx, a, env, stack, &mut r);
                r
            })
            .collect();
        stack.push(d);
        // A weak map drops its values (8).
        if Some(d) == self.weak_map {
            if let Some(v) = argr.get(1) {
                out.add(v);
            }
        } else if self.weak_map.is_some_and(|w| cx.class_extends(d, w)) {
            // A subclass of one: its base class, in terms of its own type parameters.
            if let Some(b) = cx.adt(d).and_then(|a| a.base) {
                self.reach_in(cx, b, Some(&argr), stack, out);
            }
        }
        self.weak_values(cx, t, Some(d), stack, out);
        let family = self.family.get(&d).cloned().unwrap_or_else(|| vec![d]);
        for m in family {
            if let Some(h) = self.hooks.get(&m) {
                if !h.0.is_empty() || !h.1.is_empty() {
                    out.adts.insert(m);
                }
            }
            // A relative's fields: its own type parameters are not this type's.
            let e = if m == d { Some(&argr[..]) } else { None };
            for f in field_tys(cx, m) {
                self.reach_in(cx, f, e, stack, out);
            }
        }
        stack.pop();
    }

    /// What releasing a value of type `t` (an object type `d`, or an array) as a weak map's key
    /// drops: the values of the maps it may be a key of (8).
    fn weak_values(
        &self,
        cx: &Ctx,
        t: TyId,
        d: Option<DefId>,
        stack: &mut Vec<DefId>,
        out: &mut Reach,
    ) {
        for &(k, v) in &self.weak_entries {
            let key = match k {
                None => true,
                Some(k) => {
                    k == t
                        || matches!((cx.ty.kind(k), d), (TyKind::Adt(kd, _), Some(d)) if *kd == d)
                }
            };
            if key {
                self.reach_in(cx, v, None, stack, out);
            }
        }
    }

    /// Every hook a value of type `t` may run when it is dropped, formatted, serialised or
    /// copied, with no type arguments known: a type parameter may run any class's. A function
    /// value's captures are left to the closure that captures them (module_state.rs counts what
    /// a closure's body handles where the closure is made).
    pub(crate) fn hooks_reached(&self, cx: &Ctx, t: TyId) -> Vec<DefId> {
        let r = self.reach(cx, t);
        if r.param {
            let mut all: Vec<DefId> = self
                .hooks
                .values()
                .flat_map(|(d, g)| d.iter().chain(g))
                .copied()
                .collect();
            all.sort();
            all.dedup();
            return all;
        }
        let mut v = self.hooks_of(&r, true);
        v.extend(self.hooks_of(&r, false));
        v
    }

    /// The `[Symbol.dispose]()` hooks (`dispose`) or glue hooks of the classes in `r`.
    pub(super) fn hooks_of(&self, r: &Reach, dispose: bool) -> Vec<DefId> {
        let pick = |h: &(Vec<DefId>, Vec<DefId>)| if dispose { h.0.clone() } else { h.1.clone() };
        r.adts
            .iter()
            .filter_map(|d| self.hooks.get(d))
            .flat_map(pick)
            .collect()
    }

    /// [`Reach`] of `t` with its type parameters resolved: by `frame` where known, else by the
    /// types in `args` (every type the program passes as a type argument), and function values
    /// by what closures capture.
    pub(super) fn resolve(
        &self,
        cx: &mut Ctx,
        t: TyId,
        frame: &Option<Vec<TyId>>,
        args: &FxHashSet<TyId>,
        open: &mut Option<Reach>,
    ) -> Reach {
        let t = match frame {
            Some(f) if !f.is_empty() => cx.subst(t, f),
            _ => t,
        };
        let mut r = (*self.reach(cx, t)).clone();
        if r.param || (r.fns && self.captures_reach) {
            let o = open.get_or_insert_with(|| self.open(cx, args));
            r.adts.extend(o.adts.iter().copied());
        }
        r
    }

    /// What a type parameter or a function value may reach: what any type argument the program
    /// passes and any captured value reaches.
    fn open(&self, cx: &Ctx, args: &FxHashSet<TyId>) -> Reach {
        let mut r = Reach::default();
        for &t in args.iter().chain(&self.captures) {
            r.add(&self.reach(cx, t));
        }
        r.param = false;
        r.fns = false;
        r
    }
}

/// The key and value types of every `new WeakMap<K, V>()` in the program (8), also of a
/// subclass (`new Cache()` with `class Cache extends WeakMap<K, V>`); a key `None` for one that
/// mentions a type parameter.
pub(crate) fn weak_entries(cx: &mut Ctx) -> Vec<(Option<TyId>, TyId)> {
    struct Find<'a, 'm> {
        cx: &'a Ctx<'m>,
        map: DefId,
        /// The class types made with `new` that are a weak map or extend one.
        made: Vec<TyId>,
    }
    impl crate::visit::VisitMut for Find<'_, '_> {
        fn expr(&mut self, e: &mut Expr) {
            if let E::New { def, .. } = &e.kind {
                if self.cx.class_extends(*def, self.map) && !self.made.contains(&e.ty) {
                    self.made.push(e.ty);
                }
            }
        }
    }
    let Some(map) = cx.prelude_adt("WeakMap") else {
        return vec![];
    };
    let mut made = vec![];
    for i in 0..cx.defs.len() {
        let Some(Def::Fn(mut fd)) = cx.defs[i].take() else {
            continue;
        };
        let mut f = Find {
            cx,
            map,
            made: std::mem::take(&mut made),
        };
        crate::visit::block(&mut fd.body.block, &mut f);
        made = f.made;
        cx.defs[i] = Some(Def::Fn(fd));
    }
    let mut out = vec![];
    for t in made {
        let Some((root, _)) = cx.weak_root(t) else {
            continue;
        };
        if let TyKind::Adt(_, args) = cx.ty.kind(root) {
            if let [k, v] = *args.as_slice() {
                let mut ps = vec![];
                crate::types::collect_params(&cx.ty, k, &mut ps);
                let k = ps.is_empty().then_some(k);
                if !out.contains(&(k, v)) {
                    out.push((k, v));
                }
            }
        }
    }
    out
}

fn field_tys(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Adt(a)) => a.fields.iter().map(|f| f.ty).collect(),
        Some(DefInfo::Enum(e)) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// Does an object type's literal or `new` evaluate field initializers itself (no constructor
/// runs them)?
pub(super) fn has_field_inits(cx: &Ctx, d: DefId) -> bool {
    matches!(cx.info.get(d.0 as usize), Some(DefInfo::Adt(a))
        if a.fields.iter().any(|f| f.default.is_some()) || !a.redefined.is_empty())
}

/// The arguments of intrinsic `i` whose values glue may format, serialise or deep-copy, calling
/// class hooks (1 and 2 of the module docs): all of them, or none. The match lists every
/// intrinsic, so a new one must be classified.
pub(super) fn glue_args(i: Intrinsic) -> bool {
    use Intrinsic as I;
    match i {
        I::Print
        | I::PrintErr
        | I::ToString
        | I::ErrorText
        | I::Inspect
        | I::TemplateToString
        | I::JsonStringify
        | I::Clone
        | I::Transfer
        | I::Spawn
        | I::SpawnHandled
        | I::ChanSend
        | I::ChanTrySend
        | I::SharedNew
        | I::SharedSet
        | I::MutexNew
        | I::MutexWith
        | I::RwLockNew
        | I::LockRead
        | I::PromiseAll
        | I::PromiseRace
        | I::PromiseAny
        // A member access, call or write through `any` may run any getter, method or setter of
        // the value held, and formats nothing itself; counted like a hook call on its arguments.
        | I::AnyGet
        | I::AnyIndex
        | I::AnyCall
        | I::AnyApply
        | I::AnySet
        | I::AnySetIndex
        // `key in p` runs a proxy's `has` trap (and a proxy's `get` / `set` slots its traps).
        | I::AnyHas
        // A record's `__delete` removes an entry.
        | I::AnyDelete => true,
        // Run no user code (what they drop is counted as drops; the function values they call
        // as callbacks; generator resumes apart).
        I::StrConcat
        | I::ThrowLoc
        | I::AnyTo
        | I::AnyToExact
        | I::AnyTypeof
        | I::AnyIsArray
        | I::AnyKeys
        | I::AnyTruthy
        | I::AnyTag
        | I::AnyToNumber
        | I::AnyStrictEq
        | I::AnyLooseEq
        | I::StrLen
        | I::StrByteLen
        | I::Exit
        | I::Panic
        | I::ArrayWithCapacity
        | I::ArrayLen
        | I::ArrayPush
        | I::ArrayPop
        | I::ArraySwap
        | I::ArrayRemove
        | I::ArrayTruncate
        | I::ArrayMove
        | I::ArraySetLen
        | I::ArrayReserve
        | I::ArrayPrefetch
        | I::AtomicLoad
        | I::AtomicStore
        | I::AtomicRmw
        | I::AtomicCas
        | I::AtomicCasWeak
        | I::AtomicFence
        | I::AtomicSpinHint
        | I::ThreadLocalGet
        | I::ThreadLocalSet
        | I::ArrayRotateRight
        | I::ArrayRotateLeft
        | I::ArraySwapNested
        | I::Subarray
        | I::ParamOrdering
        | I::Hash
        | I::Eq
        | I::DeepEq
        | I::KeyEq
        | I::IncludesEq
        | I::Same
        | I::SameValue
        | I::Share
        | I::FieldAbsent
        | I::FieldPresent
        | I::SpreadObject
        | I::KeyRead
        | I::KeyWrite
        | I::NeedsTransfer
        | I::NeedsDrop
        | I::MayAlias
        | I::HoldsNull
        | I::NullAs
        | I::Truthy
        | I::IsNull
        | I::IsNullish
        | I::LooseEq
        | I::MayHoldPrimitive
        | I::HoldsDynamic
        | I::HoldsDynamicOrClass
        | I::FnCapturesNothing
        | I::ClosureSibling(_)
        | I::InitSlots(..)
        | I::SimdShuffle(..)
        | I::Sqrt
        | I::Floor
        | I::Ceil
        | I::Round
        | I::Trunc
        | I::FAbs
        | I::Sleep
        | I::YieldNow
        | I::ChanReceive
        | I::ChanTryReceive
        | I::SourceLocation
        | I::PromiseWiden
        | I::PromiseSame
        | I::PerfNow
        | I::ProxyArrayCall
        | I::ProxyArrayHole
        | I::IsTypedProxy
        | I::DateNow
        | I::SharedAdd
        | I::SharedGet
        | I::SharedRead
        | I::JsonParse
        | I::JsonParseRows
        | I::JsonParseCursor
        | I::DbBind
        // The `RowSource` methods these call are not hooks; std/db calls the same methods
        // itself on the path that reads rows as JSON, in the same function, so the analysis of
        // `readRows` / `readRow` sees them.
        | I::ReadRows
        | I::ReadRow
        | I::StrCharCodeAt
        | I::StrByteAt
        | I::StrLoadU8x16
        | I::StrLoadPartialU8x16
        | I::ArrayDataPtr
        | I::Attempt
        | I::Yield
        | I::GeneratorResume
        | I::GeneratorValue
        | I::GeneratorReturn
        | I::GeneratorResumeWith
        | I::GeneratorThrow
        | I::YieldDelegate
        | I::GeneratorEmbed
        | I::AsyncGeneratorResume
        | I::AsyncGeneratorValue
        | I::AsyncGeneratorReturn
        | I::AsyncGeneratorDispose
        | I::SymbolStatic
        | I::SymbolRegistry
        // Weak maps and refs (std/prelude/weak.vlt): a value a map drops (a replaced or deleted
        // entry's, or a released key's) is counted as a drop (8 of the module docs).
        | I::WeakMapSet
        | I::WeakMapGet
        | I::WeakMapHas
        | I::WeakMapDelete
        | I::WeakMapValue
        | I::WeakRefNew
        | I::WeakRefDeref => false,
    }
}

/// Does intrinsic `i` resume a generator (4)?
pub(crate) fn resumes(i: Intrinsic) -> bool {
    matches!(
        i,
        Intrinsic::GeneratorResume
            | Intrinsic::GeneratorReturn
            | Intrinsic::GeneratorResumeWith
            | Intrinsic::GeneratorThrow
            | Intrinsic::YieldDelegate
            | Intrinsic::AsyncGeneratorResume
            | Intrinsic::AsyncGeneratorReturn
            | Intrinsic::AsyncGeneratorDispose
    )
}

/// Is `e` a place read (its value is dropped by whatever holds it, not here)?
pub(super) fn is_place(e: &Expr) -> bool {
    match &e.kind {
        E::Local(..) | E::Field { .. } | E::Index { .. } | E::Lit(_) => true,
        E::UnwrapSome(x, _) | E::Downcast(x) | E::Upcast(x) | E::UnwrapVariant { expr: x, .. } => {
            is_place(x)
        }
        _ => false,
    }
}

/// The types of the values a glue intrinsic's argument hands it (into `out`): the argument
/// itself; the arguments of a call it makes, whatever the callee (an async call's arguments
/// and receiver, `args[0]`, are transferred when it is spawned: velt_vir `async_fn/spawn.rs`,
/// `dynamic_call` too), the function value of a call through one, and a closure's captures
/// (`spawn(async () => …)`); through either branch of a conditional (`spawn_if`) and a block
/// that only holds a value.
pub(super) fn glue_types(cx: &Ctx, a: &Expr, out: &mut Vec<TyId>) {
    out.push(a.ty);
    match &a.kind {
        E::Call { callee, args } => {
            out.extend(args.iter().map(|x| x.ty));
            if let Callee::Indirect(f) = callee {
                glue_types(cx, f, out);
            }
        }
        E::New { args, .. } => out.extend(args.iter().map(|x| x.ty)),
        E::If { then, els, .. } => {
            glue_types(cx, then, out);
            glue_types(cx, els, out);
        }
        E::Block(b) if b.stmts.is_empty() => {
            if let Some(v) = &b.value {
                glue_types(cx, v, out);
            }
        }
        E::Closure(d) => {
            if let Some(Some(Def::Fn(f))) = cx.defs.get(d.0 as usize) {
                for c in &f.captures {
                    out.extend(f.body.locals.get(c.inner.0 as usize).map(|l| l.ty));
                }
            }
        }
        _ => {}
    }
}
