//! Stores across the lock boundary in a `with` callback (super module docs). A store into a
//! place rooted at a local that reaches one region (`super::regions`), of a value that may
//! reach the other, is wrapped in `Intrinsic::Transfer`: an assignment, or an argument a call
//! may store into another argument or into the function value it calls (`super::summary`:
//! `out.push(v.inner)`, `stash(out, v.inner)`). A borrowed argument is passed as a
//! transferred copy there; one the callee also modifies cannot be, and that call is an error
//! (`v.addTo(out)` where `addTo` changes `this` and pushes a part of it into `out`). A store
//! into a fresh local only grows what the local reaches, and one into a place whose type
//! cannot hold a shared value (`out: i64[]`) crosses nothing.

use std::collections::HashSet;

use velt_common::Span;

use super::regions::{crosses, holds_shared, pat_locals, Regions, IN};
use super::summary::{call_flows, outer_mode, Summaries};
use crate::body::places::{is_place, place_root};
use crate::ctx::Ctx;
use crate::hir::{
    Callee, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, Stmt, StmtKind as S,
    TyKind, UseMode,
};
use crate::visit::{self, VisitMut};

/// One pass over body `f` (of `def`): grow the regions (`rewrite: false`, repeated to a
/// fixpoint), or insert the transfers (`rewrite: true`, once, with the final regions).
/// Returns the calls that would share a part of one side with the other and cannot be fixed
/// (`rewrite` only).
pub(super) fn visit(
    cx: &mut Ctx,
    r: &mut Regions,
    s: &Summaries,
    def: DefId,
    f: &mut FnDef,
    rewrite: bool,
) -> Vec<Unfixable> {
    let mut w = Stores {
        cx,
        r,
        s,
        def,
        rewrite,
        unfixable: vec![],
    };
    visit::block(&mut f.body.block, &mut w);
    w.unfixable
}

/// A store across the lock that cannot be made a copy.
pub(super) struct Unfixable {
    pub(super) span: Span,
    pub(super) kind: Cross,
}

/// Why a store across the lock cannot be made a copy.
pub(super) enum Cross {
    /// The call also changes the argument (`inward`: an outside object stored into the
    /// value; else a part of the value stored outside).
    Changed { inward: bool },
    /// A function value whose body is not visible is given both sides.
    Opaque,
    /// The part owns a resource without `clone()`.
    Resource,
}

struct Stores<'a, 'r, 's, 'm> {
    cx: &'a mut Ctx<'m>,
    r: &'r mut Regions,
    s: &'s Summaries,
    def: DefId,
    rewrite: bool,
    unfixable: Vec<Unfixable>,
}

impl Stores<'_, '_, '_, '_> {
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
            let span = value.span;
            if let Err(kind) = self.transfer_owned(value, dest) {
                self.unfixable.push(Unfixable { span, kind });
            }
        }
    }

    /// A call: the arguments it may store into other arguments, or into the function value
    /// it calls (`super::summary::call_flows`).
    fn call(&mut self, span: Span, callee: &Callee, args: &mut [Expr]) {
        let bits: Vec<u8> = args.iter().map(|a| self.mentions(a)).collect();
        let fn_bits = match callee {
            Callee::Indirect(c) => self.mentions(c),
            _ => 0,
        };
        if !self.rewrite {
            // A closure passed to a call is given what the other arguments reach.
            for (i, a) in args.iter().enumerate() {
                if let E::Closure(n) = a.kind {
                    let others = bits.iter().enumerate().filter(|(j, _)| *j != i);
                    let b = others.fold(0, |b, (_, x)| b | x);
                    for p in self.r.params.get(&n).cloned().unwrap_or_default() {
                        self.r.add(n, p, b);
                    }
                }
            }
        }
        let mut done = HashSet::new();
        for (a, b) in call_flows(self.s, callee, args) {
            let fn_value = match (b, callee) {
                (None, Callee::Indirect(c)) => Some(c.span),
                (Some(b), _) if is_fn_value(self.cx, &args[b]) => Some(args[b].span),
                _ => None,
            };
            if let Some(at) = fn_value {
                // A function value is never given a copy (it may change what it is given): a
                // closure resolved among the bodies is checked itself, any other one may not be
                // given both sides.
                let dest = b.map_or(fn_bits, |b| self.mentions(&args[b]));
                let cross = crosses(dest, bits[a]) && !self.r.resolved.contains(&at);
                if cross && self.rewrite && done.insert(a) {
                    let kind = Cross::Opaque;
                    self.unfixable.push(Unfixable { span, kind });
                }
                continue;
            }
            let (root, dest) = match b {
                // Its body is checked where it is (`super::regions`).
                Some(b) if matches!(args[b].kind, E::Closure(_)) => continue,
                Some(b) if !holds_shared(self.cx, args[b].ty) => continue,
                Some(b) => self.dest(&args[b]),
                None => continue,
            };
            if dest == 0 {
                if let (false, Some(root)) = (self.rewrite, root) {
                    self.r.add(self.def, root, bits[a]);
                }
                continue;
            }
            if !crosses(dest, bits[a]) || !self.rewrite || !done.insert(a) {
                continue;
            }
            if let Err(kind) = self.transfer_arg(&mut args[a], dest) {
                self.unfixable.push(Unfixable { span, kind });
            }
        }
    }

    /// Wrap an owned value in `Transfer`.
    fn transfer_owned(&mut self, e: &mut Expr, dest: u8) -> Result<(), Cross> {
        let borrowed = is_place(e) && outer_mode(e) != Some(UseMode::Move);
        if self.transferable(e, dest)? && !borrowed {
            wrap(e, Intrinsic::Transfer);
        }
        Ok(())
    }

    /// A call argument crossing the boundary: an owned one is transferred, a borrowed place
    /// is passed as a transferred copy. A mutably borrowed one cannot be (the callee changes
    /// it).
    fn transfer_arg(&mut self, e: &mut Expr, dest: u8) -> Result<(), Cross> {
        if !self.transferable(e, dest)? {
            return Ok(());
        }
        match outer_mode(e) {
            Some(UseMode::Borrow) if is_place(e) => {
                wrap(e, Intrinsic::Share);
                wrap(e, Intrinsic::Transfer);
            }
            Some(UseMode::BorrowMut) if is_place(e) => {
                return Err(Cross::Changed {
                    inward: dest & IN != 0,
                })
            }
            Some(UseMode::Copy) if is_place(e) => {}
            _ => wrap(e, Intrinsic::Transfer),
        }
        Ok(())
    }

    /// Values that may reach a counted object (strings keep atomic counts; a promise is never
    /// shared, and making one there is an error, `super::promises`). A resource without
    /// `clone()` cannot be copied: stored out of the value that is an error, and stored into
    /// it, it stays shared (a callback cannot move a variable it captured; the design notes'
    /// known gaps).
    fn transferable(&mut self, e: &Expr, dest: u8) -> Result<bool, Cross> {
        if !self.cx.is_shared_value(e.ty) || self.cx.is_string_value(e.ty) {
            return Ok(false);
        }
        if !self.cx.owns_uncopyable(e.ty) {
            return Ok(true);
        }
        match dest & IN != 0 {
            true => Ok(false),
            false => Err(Cross::Resource),
        }
    }
}

/// Is `e` a function value (not a closure literal, whose body is checked where it is)?
fn is_fn_value(cx: &Ctx, e: &Expr) -> bool {
    matches!(cx.ty.kind(e.ty), TyKind::FnPtr { .. } | TyKind::Closure(_))
        && !matches!(e.kind, E::Closure(_))
}

impl VisitMut for Stores<'_, '_, '_, '_> {
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
                let callee = callee.clone();
                self.call(e.span, &callee, args);
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

/// `e` becomes `op(e)`.
pub(super) fn wrap(e: &mut Expr, op: Intrinsic) {
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
