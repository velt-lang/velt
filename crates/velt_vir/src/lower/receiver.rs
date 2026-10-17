//! Method receivers held for the call (#1188). A method borrows its receiver (`this`), and a
//! receiver read from a place another reference reaches (a module `let`, a field of a counted
//! object, an element, a variable in a shared cell) can be replaced there while the method runs:
//! `cur.bump()` where `bump` calls `reset()`, which assigns `cur = new State()`. JavaScript keeps
//! running the method on the object it was called on, so such a receiver gets a reference of its
//! own for the call (a share: the class or the interface value's data is counted), dropped after
//! the statement.
//!
//! - Interface calls: the method is chosen at run time, so the receiver is held whenever it is
//!   read from such a place (`FnLower::callee_replaceable`). Virtual calls stabilize theirs like
//!   an argument (`stable_borrow`, dispatch.rs).
//! - Direct calls (class methods, accessors) stabilize the receiver with the arguments
//!   (`stable_borrow`), which shares one rooted in module state. A receiver whose only route to
//!   other code is its module state slot (no counted object, no cell on the way) stays borrowed
//!   in place when the method cannot reach module state ([`Cx::may_reach_slots`]) and no other
//!   argument can change it: `rng.next()` on a module `let rng` costs nothing.
//!
//! A receiver held in a local, a parameter or `this` is reached by no other reference while the
//! call runs, and is passed as before.

use std::collections::{HashMap, HashSet};

use velt_sema::effects::runs_user_code;
use velt_sema::hir::{self, Callee, DefId, ExprKind as E, StmtKind as S, TyKind};

use super::frame_envs::children;
use super::sequence::later_each;
use super::{Cx, FnLower};
use crate::vir::Operand;

impl FnLower<'_, '_> {
    /// Is the receiver `recv` of a method call read from a place the method may replace, as a
    /// class object or an interface value a share keeps alive?
    pub(super) fn replaceable_receiver(&mut self, recv: &hir::Expr) -> bool {
        let ty = self.sub(recv.ty);
        let shareable = self.cx.is_class(ty) || matches!(self.cx.kind(ty), TyKind::Dyn(..));
        shareable && !super::call::is_moved(recv) && self.callee_replaceable(recv)
    }

    /// The receiver `recv` with a reference of its own for the call, dropped after the
    /// statement.
    pub(super) fn held_receiver(&mut self, recv: &hir::Expr) -> Operand {
        let ty = self.sub(recv.ty);
        let v = self.expr(recv);
        let s = self.share_value(v, ty);
        self.own_value(s, ty)
    }

    /// Can the class receiver `args[0]` of the direct method call of `def` be borrowed in place:
    /// rooted in a module state slot through no counted object, the method cannot reach module
    /// state, and the other arguments neither change memory nor are read from module state?
    pub(super) fn receiver_in_place(&mut self, def: DefId, args: &[hir::Expr]) -> bool {
        let Some((recv, rest)) = args.split_first() else {
            return false;
        };
        let ty = self.sub(recv.ty);
        if !self.cx.is_class(ty)
            || super::call::is_moved(recv)
            || !self.rooted_in_slot(recv)
            || later_each(args)[0].any()
            || rest.iter().any(|a| self.rooted_in_slot(a))
            || self.through_counted(recv, ty)
        {
            return false;
        }
        // An argument the method drops may run a dispose hook (`cur.f(d)` with `d` moved in).
        !self.cx.may_reach_slots(def) && !rest.iter().any(|a| self.cx.drop_reaches(a.ty))
    }
}

/// What [`Cx::may_reach_slots`] has found so far.
#[derive(Default)]
pub(super) struct Reach {
    /// Per function; a function still being walked counts as reaching module state.
    fns: HashMap<DefId, bool>,
    /// Per class, for `new` (its constructor and field initializers); a class still being walked
    /// counts as reaching module state.
    news: HashMap<DefId, bool>,
    /// Whether some glue hook (`toString`, `toJSON`, `__inspect`, `[Symbol.dispose]`) may reach
    /// module state, once known.
    hooks: Option<bool>,
    /// The classes, structs and enums whose drop may run a `[Symbol.dispose]` hook that reaches
    /// module state (their own, a part's, or a subclass's), known with `hooks`.
    drops: HashSet<DefId>,
    /// Per type: may dropping a value of it run such a hook?
    drop_tys: HashMap<hir::TyId, bool>,
}

impl Cx<'_> {
    /// Can a call of function `def` run code that reaches a module state slot, and so replace
    /// what a receiver read from one holds? It can when its body (or a function it calls
    /// directly, transitively) names a slot, calls through a function value, a vtable or an
    /// interface, awaits, iterates something other than an array or a string, or hands a
    /// function value, an interface value or `any` to an intrinsic, which may call it. When a
    /// glue hook (`toString`, `toJSON`, `__inspect`, dispose) may reach module state itself, an
    /// intrinsic handed any object may run it. A value whose drop may run a dispose hook that
    /// reaches module state counts wherever it appears (`drop_reaches`). Constructors and field
    /// initializers of `new` count as calls.
    pub(super) fn may_reach_slots(&mut self, def: DefId) -> bool {
        self.hooks();
        self.fn_reaches(def)
    }

    fn fn_reaches(&mut self, def: DefId) -> bool {
        if let Some(&r) = self.reach.fns.get(&def) {
            return r;
        }
        // A recursive call is assumed to reach module state while its result is unknown.
        self.reach.fns.insert(def, true);
        let hir = self.hir;
        let r = match hir.def(def) {
            // Drops run dispose hooks where the walk sees no call (`using` at a block's end, a
            // field or variable overwritten, a parameter dropped at the end): a function with a
            // variable or parameter whose drop may run one that reaches module state may too.
            hir::Def::Fn(f) => {
                f.is_async
                    || f.is_generator
                    || f.body.locals.iter().any(|l| self.drop_reaches(l.ty))
                    || self.block_reaches(&f.body.block)
            }
            _ => true,
        };
        self.reach.fns.insert(def, r);
        r
    }

    /// May a glue hook of the program reach module state? Found once, with hooks assumed not to
    /// while their own bodies are walked (a hook another hook's glue runs is one of those
    /// walked); what that walk learned about other functions is then forgotten, since it
    /// assumed no hook reaches module state.
    fn hooks(&mut self) -> bool {
        if let Some(h) = self.reach.hooks {
            return h;
        }
        self.reach.hooks = Some(false);
        let hir = self.hir;
        let mut glue = vec![];
        let mut dispose = vec![];
        for (i, d) in hir.defs.iter().enumerate() {
            if let hir::Def::Adt(a) = d {
                dispose.extend(a.dispose.map(|h| (DefId(i as u32), h)));
                glue.extend([a.to_string, a.to_json, a.inspect].into_iter().flatten());
            }
        }
        let disposing: Vec<DefId> = dispose
            .into_iter()
            .filter(|&(_, hook)| self.fn_reaches(hook))
            .map(|(owner, _)| owner)
            .collect();
        let h = !disposing.is_empty() || glue.into_iter().any(|d| self.fn_reaches(d));
        self.reach.drops = drop_closure(hir, disposing);
        self.reach.fns.clear();
        self.reach.news.clear();
        self.reach.hooks = Some(h);
        h
    }

    fn block_reaches(&mut self, b: &hir::Block) -> bool {
        for s in &b.stmts {
            let r = match &s.kind {
                S::Let { init, .. } => init.as_ref().is_some_and(|e| self.expr_reaches(e)),
                S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => {
                    self.expr_reaches(init)
                }
                S::Return(None) | S::Break(_) | S::Continue(_) => false,
                S::If { cond, then, els } => {
                    self.expr_reaches(cond)
                        || self.block_reaches(then)
                        || els.as_ref().is_some_and(|b| self.block_reaches(b))
                }
                S::While {
                    cond, body, step, ..
                } => {
                    self.expr_reaches(cond)
                        || self.block_reaches(body)
                        || step.as_ref().is_some_and(|e| self.expr_reaches(e))
                }
                S::ForOf { iter, body, .. } => {
                    !matches!(self.types.kind(iter.ty), TyKind::Array(_) | TyKind::Str)
                        || self.expr_reaches(iter)
                        || self.block_reaches(body)
                }
                S::Try {
                    body,
                    catch,
                    finally,
                } => {
                    self.block_reaches(body)
                        || catch.as_ref().is_some_and(|(_, b)| self.block_reaches(b))
                        || finally.as_ref().is_some_and(|b| self.block_reaches(b))
                }
                S::Block(b) => self.block_reaches(b),
            };
            if r {
                return true;
            }
        }
        b.value.as_ref().is_some_and(|e| self.expr_reaches(e))
    }

    fn expr_reaches(&mut self, e: &hir::Expr) -> bool {
        // A value whose drop may run a dispose hook that reaches module state: it may be dropped
        // here (a temporary, the old value of an assigned place, a value moved into a callee).
        if self.drop_reaches(e.ty) {
            return true;
        }
        let here = match &e.kind {
            E::Global(d) => matches!(self.hir.def(*d), hir::Def::Global(g) if g.slot.is_some()),
            E::Await(_) => true,
            E::Block(b) => return self.block_reaches(b),
            E::Call { callee, args } => match callee {
                Callee::Def(d, _) => match self.hir.def(*d) {
                    hir::Def::Fn(_) => self.fn_reaches(*d),
                    _ => true,
                },
                Callee::Intrinsic(i) => {
                    runs_user_code(*i) || args.iter().any(|a| self.may_call(a.ty))
                }
                Callee::Indirect(_)
                | Callee::Virtual { .. }
                | Callee::Dyn { .. }
                | Callee::ParamMethod { .. } => true,
            },
            E::New { def, .. } => self.new_reaches(*def),
            _ => false,
        };
        if here {
            return true;
        }
        let mut r = false;
        children(e, &mut |x| r = r || self.expr_reaches(x));
        r
    }

    /// Can an intrinsic handed a value of type `t` run user code with it: a function value, an
    /// interface value or `any` (or a generic type that may be one), or, when a glue hook may
    /// reach module state, any value that is not a number, a boolean or a string?
    fn may_call(&mut self, t: hir::TyId) -> bool {
        match self.types.kind(t) {
            TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Dyn(..) | TyKind::Param(_) => true,
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Unit => false,
            _ => self.reach.hooks.unwrap_or(true),
        }
    }

    /// May dropping a value of type `t` run a dispose hook that reaches module state? A value
    /// of an unknown type (a type parameter, an interface or function value) may hold one when
    /// any such hook exists.
    pub(super) fn drop_reaches(&mut self, t: hir::TyId) -> bool {
        if self.reach.drops.is_empty() {
            return false;
        }
        if let Some(&r) = self.reach.drop_tys.get(&t) {
            return r;
        }
        let r = match self.types.kind(t).clone() {
            TyKind::Adt(d, args) => {
                self.reach.drops.contains(&d) || args.into_iter().any(|a| self.drop_reaches(a))
            }
            TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => self.drop_reaches(x),
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                self.drop_reaches(a) || self.drop_reaches(b)
            }
            TyKind::Tuple(xs) => xs.into_iter().any(|x| self.drop_reaches(x)),
            TyKind::Param(_)
            | TyKind::Dyn(..)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_)
            | TyKind::Op(..) => true,
            _ => false,
        };
        self.reach.drop_tys.insert(t, r);
        r
    }

    /// Does `new` of class `def` run code that reaches module state (its constructor, its
    /// fields' initializers, its base class's)? Memoized per class: field initializers may
    /// create the class itself (`next: Link | null = c ? new Link() : null`) or each other.
    fn new_reaches(&mut self, def: DefId) -> bool {
        if let Some(&r) = self.reach.news.get(&def) {
            return r;
        }
        self.reach.news.insert(def, true);
        let r = self.new_walk(def);
        self.reach.news.insert(def, r);
        r
    }

    fn new_walk(&mut self, def: DefId) -> bool {
        let hir = self.hir;
        let hir::Def::Adt(a) = hir.def(def) else {
            return true;
        };
        if a.ctor.is_some_and(|c| self.fn_reaches(c)) {
            return true;
        }
        // A base class's constructor runs too when this class declares none.
        if let Some(TyKind::Adt(b, _)) = a.base.map(|b| self.types.kind(b).clone()) {
            if self.new_reaches(b) {
                return true;
            }
        }
        let defaults = a.fields.iter().filter_map(|f| f.default.as_ref());
        let inherited = a.inherited_inits.iter().map(|i| &i.init);
        let inits: Vec<&hir::Expr> = defaults.chain(inherited).collect();
        inits.into_iter().any(|e| self.expr_reaches(e))
    }
}

/// The types whose drop may run one of the dispose hooks of `disposing` (the types declaring
/// them): those types, the types with a field or variant payload of such a type (a type
/// parameter in a declaration stands for an argument, checked where the type is used), and the
/// base classes of such classes (a base-class value may be the subclass). Found by iterating to
/// a fixed point over the program's types.
fn drop_closure(hir: &hir::Program, disposing: Vec<DefId>) -> HashSet<DefId> {
    let mut set: HashSet<DefId> = disposing.into_iter().collect();
    if set.is_empty() {
        return set;
    }
    loop {
        let before = set.len();
        for (i, d) in hir.defs.iter().enumerate() {
            let id = DefId(i as u32);
            match d {
                hir::Def::Adt(a) => {
                    let mine = set.contains(&id) || a.fields.iter().any(|f| holds(hir, f.ty, &set));
                    if mine {
                        set.insert(id);
                        if let Some(TyKind::Adt(b, _)) = a.base.map(|b| hir.types.kind(b)) {
                            set.insert(*b);
                        }
                    }
                }
                hir::Def::Enum(e) => {
                    let parts = e.variants.iter().flat_map(|v| v.payload.iter());
                    if parts.into_iter().any(|&p| holds(hir, p, &set)) {
                        set.insert(id);
                    }
                }
                _ => {}
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

/// Does the declared type `t` hold a value of a type in `set` (a type parameter stands for an
/// argument, checked where the type is used)?
fn holds(hir: &hir::Program, t: hir::TyId, set: &HashSet<DefId>) -> bool {
    match hir.types.kind(t) {
        TyKind::Adt(d, args) => set.contains(d) || args.iter().any(|&a| holds(hir, a, set)),
        TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => holds(hir, *x, set),
        TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
            holds(hir, *a, set) || holds(hir, *b, set)
        }
        TyKind::Tuple(xs) => xs.iter().any(|&x| holds(hir, x, set)),
        TyKind::Dyn(..) | TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
        _ => false,
    }
}
