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

use std::collections::HashMap;

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
        !self.cx.may_reach_slots(def)
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
}

impl Cx<'_> {
    /// Can a call of function `def` run code that reaches a module state slot, and so replace
    /// what a receiver read from one holds? It can when its body (or a function it calls
    /// directly, transitively) names a slot, calls through a function value, a vtable or an
    /// interface, awaits, iterates something other than an array or a string, or hands a
    /// function value, an interface value or `any` to an intrinsic, which may call it. When a
    /// glue hook (`toString`, `toJSON`, `__inspect`, dispose) may reach module state itself, an
    /// intrinsic handed any object may run it. Constructors and field initializers of `new`
    /// count as calls.
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
            hir::Def::Fn(f) => f.is_async || f.is_generator || self.block_reaches(&f.body.block),
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
        let hooks: Vec<DefId> = hir
            .defs
            .iter()
            .filter_map(|d| match d {
                hir::Def::Adt(a) => Some([a.dispose, a.to_string, a.to_json, a.inspect]),
                _ => None,
            })
            .flatten()
            .flatten()
            .collect();
        let h = hooks.into_iter().any(|d| self.fn_reaches(d));
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
