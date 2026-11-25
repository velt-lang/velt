//! Calls of methods that hand back their receiver (`crate::hand_back`): the method returns
//! nothing, and the call is a block that runs it and then gives the receiver,
//! `{ s.add(x); s }`. The receiver is read again when it is a place that reads the same value
//! without running anything (`s`, `this.items`); a temporary (`new Set<number>().add(1)`) is
//! kept in a local first. Where the result goes decides what that read is:
//! - unused (`s.add(x);`): nothing, just the call ([`FnCx::discard_hand_back`]);
//! - the receiver of a member access (`s.add(1).add(2)`): a borrow, after the call
//!   ([`FnCx::split_hand_back`], as `in_place_chain` does for `sort`);
//! - a value (`const t = s.add(3)`, an argument, `return`): the place used as the context wants
//!   it ([`FnCx::hand_back_want`]), so a kept result is a share only where the receiver stays in
//!   use, as `const t = s` would be.
//!
//! In the method's own body, `return this;` (or `return super.add(x);`) becomes `return;`
//! ([`FnCx::hand_back_returns`]).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, DefId, ExprKind as H, LocalId, PassMode, StmtKind as S, TyId, UseMode};
use crate::visit::{self, VisitMut};

impl FnCx<'_, '_> {
    /// `call` (of method `def`, which hands back its receiver, on a receiver of type `to`) as
    /// the block that runs it and then gives the receiver.
    pub(super) fn hand_back_call(
        &mut self,
        mut call: hir::Expr,
        def: DefId,
        to: TyId,
    ) -> hir::Expr {
        let span = call.span;
        let unit = self.cx.ty.unit;
        let H::Call { args, .. } = &mut call.kind else {
            return call;
        };
        let Some((first, rest)) = args.split_first_mut() else {
            return call;
        };
        let recv = unconverted(first);
        // The call is an expression statement: nothing to give back.
        if self.discard_at == Some(span) {
            call.ty = unit;
            self.handed_back.insert(span, true);
            let block = hir::Block {
                stmts: vec![stmt(S::Expr(call), span)],
                value: None,
                span,
            };
            return self.mk(H::Block(block), unit, span);
        }
        let mut stmts = vec![];
        let place = self.stays(recv, rest);
        let value = if place {
            let mut again = recv.clone();
            set_place_mode(&mut again, UseMode::Move);
            again
        } else {
            let ty = recv.ty;
            let tmp = self.new_local("<receiver>", ty, true, span, LocalKind::Temp);
            // The temporary is passed as the receiver was: changed when the place was read
            // mutably or the method changes `this`.
            let mutates = self.this_mode(def) == PassMode::BorrowMut;
            let mode = match mutates || place_mode(recv) == Some(UseMode::BorrowMut) {
                true => UseMode::BorrowMut,
                false => UseMode::Borrow,
            };
            let read = self.mk(H::Local(tmp, mode), ty, span);
            let mut init = std::mem::replace(recv, read);
            set_place_mode(&mut init, UseMode::Move);
            stmts.push(stmt(
                S::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            ));
            self.mk(H::Local(tmp, UseMode::Move), ty, span)
        };
        call.ty = unit;
        stmts.push(stmt(S::Expr(call), span));
        let ty = if self.cx.ty.is_bottom(value.ty) {
            to
        } else {
            value.ty
        };
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.handed_back.insert(span, place);
        self.mk(H::Block(block), ty, span)
    }

    /// Does `recv`, the receiver of a call with arguments `args`, still hold the receiver once
    /// the call returns, so that reading it again gives the receiver? `this`, a module
    /// constant, or a local that no closure and no argument assigns: nothing the call runs can
    /// assign them. A field or element (or a module `let`) can be assigned by the method, by a
    /// closure it calls or by an argument, so the receiver is kept as it was read, before the
    /// arguments.
    fn stays(&self, recv: &hir::Expr, args: &mut [hir::Expr]) -> bool {
        match recv.kind {
            H::Global(d) => self.cx.global(d).is_some_and(|g| !g.mutable),
            H::Local(l, _) if self.local_kind(l) == LocalKind::This => true,
            H::Local(l, _) => self.closure_assignment(l).is_none() && !assigns(args, l),
            _ => false,
        }
    }

    /// `e` checked as an expression statement (`expr_stmt`): a call of a method that hands
    /// back its receiver there needs no receiver back.
    pub(crate) fn discarded_expr(&mut self, e: &ast::Expr) -> hir::Expr {
        let outer = self.discard_at.replace(e.span);
        let h = self.expr(e, None, Want::Borrow);
        self.discard_at = outer;
        h
    }

    /// Is `h` the block of a call that hands back its receiver?
    pub(crate) fn is_hand_back(&self, h: &hir::Expr) -> bool {
        matches!(h.kind, H::Block(_)) && self.handed_back.contains_key(&h.span)
    }

    /// `h` (checked with `want`) with the receiver it hands back read as `want` says, when it
    /// reads a place again (also inside the blocks of the temporaries its arguments needed).
    pub(super) fn hand_back_want(&self, mut h: hir::Expr, want: Want) -> hir::Expr {
        let mode = match want {
            Want::Move => UseMode::Move,
            Want::Borrow => UseMode::Borrow,
            Want::BorrowMut => UseMode::BorrowMut,
        };
        // Down the blocks around it (a chain's earlier calls, its arguments' temporaries).
        let mut cur = &mut h;
        loop {
            let marked = self.handed_back.get(&cur.span).copied();
            let H::Block(b) = &mut cur.kind else {
                break;
            };
            let Some(value) = b.value.as_deref_mut() else {
                break;
            };
            if matches!(value.kind, H::Block(_)) {
                cur = value;
                continue;
            }
            if marked == Some(true) {
                set_place_mode(value, mode);
            }
            break;
        }
        h
    }

    /// `h` (an expression statement) without the receiver a call that hands it back gives:
    /// the call alone, or with the earlier calls of its chain and the temporary holding the
    /// receiver.
    pub(crate) fn discard_hand_back(&self, h: hir::Expr) -> hir::Expr {
        if !self.is_hand_back(&h) {
            return h;
        }
        let unit = self.cx.ty.unit;
        let (span, H::Block(mut b)) = (h.span, h.kind) else {
            unreachable!("ICE: a hand-back call is a block");
        };
        // A temporary holding the receiver: the call takes it as it was.
        if self.handed_back.get(&span) == Some(&false) {
            untemp(&mut b.stmts);
        }
        // The receiver read is dropped; a block there (the chain's last call) runs.
        match b.value.take().map(|v| *v) {
            Some(v) if matches!(v.kind, H::Block(_)) => {
                let inner = self.discard_hand_back(v);
                match inner.kind {
                    H::Block(ib) if ib.value.is_none() => b.stmts.extend(ib.stmts),
                    _ => b.stmts.push(stmt(S::Expr(inner), span)),
                }
            }
            _ => {}
        }
        if let [hir::Stmt {
            kind: S::Expr(_), ..
        }] = b.stmts.as_slice()
        {
            if let Some(S::Expr(call)) = b.stmts.pop().map(|s| s.kind) {
                return call;
            }
        }
        hir::Expr {
            kind: H::Block(b),
            ty: unit,
            span,
        }
    }

    /// `recv`, the receiver of a member access, split when it is a call that hands back its
    /// receiver: the statements that run first and the receiver they leave, borrowed.
    pub(super) fn split_hand_back(
        &self,
        recv: hir::Expr,
    ) -> Result<(Vec<hir::Stmt>, hir::Expr), hir::Expr> {
        if !self.is_hand_back(&recv) {
            return Err(recv);
        }
        let H::Block(b) = recv.kind else {
            unreachable!("ICE: a hand-back call is a block");
        };
        let mut before = b.stmts;
        let value = *b.value.expect("ICE: a hand-back call gives its receiver");
        // A chain (`a.f().g()` as a receiver): the earlier calls' statements first.
        match self.split_hand_back(value) {
            Ok((more, recv)) => {
                before.extend(more);
                Ok((before, recv))
            }
            Err(mut v) => {
                set_place_mode(&mut v, UseMode::Borrow);
                Ok((before, v))
            }
        }
    }

    /// The body (`block`) of a method that hands back its receiver: each `return this;` is
    /// `return;`, and `return super.m(x);` (a call handing back `this`) runs the call, then
    /// returns.
    pub(crate) fn hand_back_returns(&self, block: &mut hir::Block) {
        let unit = self.cx.ty.unit;
        let mut v = Returns { cx: self, unit };
        visit::block(block, &mut v);
        // The body's end returns anyway.
        if matches!(block.stmts.last(), Some(s) if matches!(s.kind, S::Return(None))) {
            block.stmts.pop();
        }
    }
}

/// Rewrites the `return`s of a method that hands back its receiver (closures are bodies of
/// their own, not visited).
struct Returns<'a, 'b, 'c> {
    cx: &'a FnCx<'b, 'c>,
    unit: TyId,
}

impl VisitMut for Returns<'_, '_, '_> {
    fn stmt(&mut self, s: &mut hir::Stmt) {
        if !matches!(s.kind, S::Return(Some(_))) {
            return;
        }
        let S::Return(Some(e)) = std::mem::replace(&mut s.kind, S::Return(None)) else {
            return;
        };
        if is_receiver(&e) {
            return;
        }
        let run = self.cx.discard_hand_back(e);
        let span = s.span;
        let block = hir::Block {
            stmts: vec![stmt(S::Expr(run), span), stmt(S::Return(None), span)],
            value: None,
            span,
        };
        s.kind = S::Expr(hir::Expr {
            kind: H::Block(block),
            ty: self.unit,
            span,
        });
    }
}

/// `stmts`, `let tmp = <receiver>; m(tmp, …);`, as `m(<receiver>, …);`.
fn untemp(stmts: &mut Vec<hir::Stmt>) {
    let [hir::Stmt {
        kind: S::Let {
            local,
            init: Some(_),
        },
        ..
    }, hir::Stmt {
        kind: S::Expr(call),
        ..
    }] = stmts.as_mut_slice()
    else {
        return;
    };
    let H::Call { args, .. } = &mut call.kind else {
        return;
    };
    let Some(arg) = args.first_mut().map(unconverted) else {
        return;
    };
    let H::Local(l, mode) = arg.kind else {
        return;
    };
    if l != *local {
        return;
    }
    let first = stmts.remove(0);
    let S::Let {
        init: Some(mut init),
        ..
    } = first.kind
    else {
        return;
    };
    set_place_mode(&mut init, mode);
    if let S::Expr(call) = &mut stmts[0].kind {
        if let H::Call { args, .. } = &mut call.kind {
            if let Some(arg) = args.first_mut().map(unconverted) {
                *arg = init;
            }
        }
    }
}

/// `this` itself (as its class or a base class).
fn is_receiver(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Local(..) => true,
        H::Upcast(x) | H::Downcast(x) => is_receiver(x),
        _ => false,
    }
}

/// Does one of `args` assign local `l` itself (`s.add(s = other)`)?
fn assigns(args: &mut [hir::Expr], l: LocalId) -> bool {
    let mut found = false;
    for a in args {
        visit::exprs_in(a, &mut |e| {
            if let H::Assign { place, .. } | H::CompoundAssign { place, .. } = &e.kind {
                found |= matches!(place.kind, H::Local(p, _) if p == l);
            }
        });
    }
    found
}

/// How place `e` is read (`None`: `e` is no place).
fn place_mode(e: &hir::Expr) -> Option<UseMode> {
    match &e.kind {
        H::Local(_, mode)
        | H::Field { mode, .. }
        | H::Index { mode, .. }
        | H::UnwrapSome(_, mode)
        | H::UnwrapVariant { mode, .. } => Some(*mode),
        H::Downcast(x) => place_mode(x),
        _ => None,
    }
}

/// The receiver as the call passes it: under its conversion to the class declaring the method.
fn unconverted(e: &mut hir::Expr) -> &mut hir::Expr {
    match e.kind {
        H::Upcast(ref mut x) => unconverted(x),
        _ => e,
    }
}

fn stmt(kind: S, span: Span) -> hir::Stmt {
    hir::Stmt { kind, span }
}
