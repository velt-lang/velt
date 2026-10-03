//! Stores across the lock boundary in a `with` callback (super module docs). A store into a
//! place rooted at a local that reaches one region (`super::regions`), of a value that may
//! reach the other, is wrapped in `Intrinsic::Transfer`: an assignment, or an argument of a
//! call that may modify another argument (`out.push(v.inner)`; a borrowed argument is passed
//! as a transferred copy there, since the callee may keep a share of it). A store into a fresh
//! local only grows what the local reaches.

use std::collections::HashSet;

use super::regions::{crosses, pat_locals, Regions, IN};
use crate::body::places::{is_place, place_root};
use crate::ctx::Ctx;
use crate::hir::{
    Callee, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, Stmt, StmtKind as S,
    UseMode,
};
use crate::visit::{self, VisitMut};

/// One pass over body `f` (of `def`): grow the regions (`rewrite: false`, repeated to a
/// fixpoint), or insert the transfers (`rewrite: true`, once, with the final regions).
pub(super) fn visit(cx: &mut Ctx, r: &mut Regions, def: DefId, f: &mut FnDef, rewrite: bool) {
    let mut w = Stores {
        cx,
        r,
        def,
        rewrite,
    };
    visit::block(&mut f.body.block, &mut w);
}

struct Stores<'a, 'r, 'm> {
    cx: &'a mut Ctx<'m>,
    r: &'r mut Regions,
    def: DefId,
    rewrite: bool,
}

impl Stores<'_, '_, '_> {
    fn mentions(&mut self, e: &Expr) -> u8 {
        self.r.mentions(self.cx, self.def, e)
    }

    /// Bind the locals of `p` to what `b` reaches.
    fn bind(&mut self, p: &Pat, b: u8) {
        let mut ls = vec![];
        pat_locals(p, &mut ls);
        for l in ls {
            self.r.add(self.def, l, b);
        }
    }

    /// What a store into `place` reaches: its root local's regions, or (a place in a call's
    /// result) what the place expression mentions.
    fn dest(&mut self, place: &Expr) -> (Option<LocalId>, u8) {
        match place_root(place) {
            Some(root) => (Some(root), self.r.get(self.def, root)),
            None => (None, self.mentions(place)),
        }
    }

    /// `place = value`.
    fn assign(&mut self, place: &Expr, value: &mut Expr) {
        let b = self.mentions(value);
        let (root, dest) = self.dest(place);
        let rebind = matches!(place.kind, E::Local(..))
            && root.is_some_and(|l| !self.r.is_home(self.def, l));
        if rebind || dest == 0 || !crosses(dest, b) {
            if let (false, Some(root)) = (self.rewrite, root) {
                self.r.add(self.def, root, b);
            }
        } else if self.rewrite {
            self.transfer_owned(value, dest);
        }
    }

    /// A call: each argument it may modify, and a function value it calls (which may store
    /// what it is given into what it captured), may receive the other arguments.
    fn call(&mut self, callee: Option<&Expr>, args: &mut [Expr]) {
        let bits: Vec<u8> = args.iter().map(|a| self.mentions(a)).collect();
        let others = |i: Option<usize>| {
            let rest = bits.iter().enumerate().filter(|(j, _)| Some(*j) != i);
            rest.fold(0, |b, (_, x)| b | x)
        };
        if !self.rewrite {
            for (i, a) in args.iter().enumerate() {
                if let E::Closure(n) = a.kind {
                    for p in self.r.params.get(&n).cloned().unwrap_or_default() {
                        self.r.add(n, p, others(Some(i)));
                    }
                }
            }
        }
        let mut dests: Vec<(Option<usize>, Option<LocalId>, u8)> = vec![];
        if let Some(c) = callee {
            dests.push((None, None, self.mentions(c)));
        }
        for (i, a) in args.iter().enumerate() {
            if is_place(a) && outer_mode(a) == Some(UseMode::BorrowMut) {
                let (root, dest) = self.dest(a);
                dests.push((Some(i), root, dest));
            }
        }
        let mut done = HashSet::new();
        for (i, root, dest) in dests {
            if dest == 0 {
                if let (false, Some(root)) = (self.rewrite, root) {
                    self.r.add(self.def, root, others(i));
                }
                continue;
            }
            for j in 0..args.len() {
                if Some(j) != i && crosses(dest, bits[j]) && self.rewrite && done.insert(j) {
                    self.transfer_arg(&mut args[j], dest);
                }
            }
        }
    }

    /// Wrap an owned value in `Transfer`.
    fn transfer_owned(&mut self, e: &mut Expr, dest: u8) {
        let borrowed = is_place(e) && outer_mode(e) != Some(UseMode::Move);
        if self.transferable(e, dest) && !borrowed {
            wrap(e, Intrinsic::Transfer);
        }
    }

    /// A call argument crossing the boundary: an owned one is transferred, a borrowed place
    /// is passed as a transferred copy; a mutably borrowed one stays (the callee changes it).
    fn transfer_arg(&mut self, e: &mut Expr, dest: u8) {
        if !self.transferable(e, dest) {
            return;
        }
        match outer_mode(e) {
            Some(UseMode::Borrow) if is_place(e) => {
                wrap(e, Intrinsic::Share);
                wrap(e, Intrinsic::Transfer);
            }
            Some(UseMode::Borrow | UseMode::BorrowMut | UseMode::Copy) if is_place(e) => {}
            _ => wrap(e, Intrinsic::Transfer),
        }
    }

    /// Values that may reach a counted object (strings keep atomic counts; a promise is never
    /// shared, and making one there is an error, `super::promises`). A resource without
    /// `clone()` stored into the value stays shared: it cannot be copied, and a callback cannot
    /// move a variable it captured (the design notes' known gaps).
    fn transferable(&mut self, e: &Expr, dest: u8) -> bool {
        let inward = dest & IN != 0;
        self.cx.is_shared_value(e.ty)
            && !self.cx.is_string_value(e.ty)
            && !(inward && self.cx.owns_uncopyable(e.ty))
    }
}

impl VisitMut for Stores<'_, '_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        if self.rewrite {
            return;
        }
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } => {
                let b = self.mentions(init);
                self.r.add(self.def, *local, b);
            }
            S::LetPat { pat, init } => {
                let b = self.mentions(init);
                self.bind(pat, b);
            }
            S::ForOf { binding, iter, .. } => {
                let b = self.mentions(iter);
                self.bind(binding, b);
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            E::Assign { place, value } => {
                let place = (**place).clone();
                self.assign(&place, value);
            }
            // The transfers inserted here are not stores.
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Transfer | Intrinsic::Share),
                ..
            } => {}
            E::Call { callee, args } => {
                let c = match callee {
                    Callee::Indirect(c) => Some((**c).clone()),
                    _ => None,
                };
                self.call(c.as_ref(), args);
            }
            E::Match { scrutinee, arms } if !self.rewrite => {
                let b = self.mentions(scrutinee);
                let pats: Vec<Pat> = arms.iter().map(|a| a.pat.clone()).collect();
                for p in &pats {
                    self.bind(p, b);
                }
            }
            _ => {}
        }
    }
}

/// The use mode of a place's outermost node.
fn outer_mode(e: &Expr) -> Option<UseMode> {
    match &e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => Some(*m),
        _ => None,
    }
}

/// `e` becomes `op(e)`.
fn wrap(e: &mut Expr, op: Intrinsic) {
    let (ty, span) = (e.ty, e.span);
    let unit = Expr {
        kind: E::Lit(crate::hir::Lit::Unit),
        ty,
        span,
    };
    let inner = std::mem::replace(e, unit);
    *e = Expr {
        kind: E::Call {
            callee: Callee::Intrinsic(op),
            args: vec![inner],
        },
        ty,
        span,
    };
}
