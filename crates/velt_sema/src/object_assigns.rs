//! Which function results and fields of an object type fill an interface's
//! (`crate::object_ifaces`): those a plain assignment `const x: Want = value` takes, checked and
//! converted by the same coercion (`FnCx::try_coerce`). A result `{ v } | undefined` for
//! `Item | undefined`, `{ a: { v } }` for `{ a: Item }` and `undefined` for `number | undefined`
//! fit; a field fills the interface's when the assignment keeps the object itself (an `Upcast`
//! of an object of the same layout, however deep), so a write into a part of it reaches the
//! object, as in Node.

use velt_common::Span;

use crate::body::recheck::Mark;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{self, Def, DefId, ExprKind as H, LocalId, StmtKind, TyId};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// Does a plain assignment take a value of type `from` as a `want` (in `module`)? Tried and
    /// rolled back: `Some(true)` when the value stays the same object (or is the value itself),
    /// `Some(false)` when it becomes another value made of it, `None` when it does not convert.
    pub(crate) fn assigns(&mut self, from: TyId, want: TyId, module: usize) -> Option<bool> {
        if self.same_ty(from, want) {
            return Some(true);
        }
        let mark = Mark::here(self);
        let env = TyEnv::new(module, &[]);
        let mut g = FnCx::new(self, module, env, Frame::new(FnKind::Free, None));
        let span = Span::DUMMY;
        let l = g.new_local("#value", from, false, span, LocalKind::Temp);
        let mode = g.use_mode(from, Want::Move);
        let h = g.mk(H::Local(l, mode), from, span);
        let diags = g.cx.diags.len();
        let r = g.try_coerce(h, want);
        let clean = g.cx.diags.len() == diags;
        drop(g);
        mark.rollback(self);
        match r {
            Ok(e) if clean => Some(same_object(&e, l)),
            _ => None,
        }
    }

    /// Make synthesized method `def` (of object type `d`, its body `return <call>;` whose call
    /// gives a `gives`) return the call's value converted to its result type `want`, as a plain
    /// assignment converts it (`Ctx::assigns` said it does).
    pub(crate) fn assign_result(&mut self, def: DefId, d: DefId, gives: TyId, want: TyId) {
        let module = self.adt(d).map_or(0, |a| a.module);
        let Some(Def::Fn(f)) = &mut self.defs[def.0 as usize] else {
            return;
        };
        let span = f.span;
        let old_locals = f.body.locals.clone();
        let mut stmts = std::mem::take(&mut f.body.block.stmts);
        let env = TyEnv::new(module, &[]);
        let mut g = FnCx::new(self, module, env, Frame::new(FnKind::Method, Some(want)));
        g.body_def = Some(def);
        g.owner = Some(d);
        g.cx.checking.push(def);
        for (k, l) in old_locals.iter().enumerate() {
            let kind = match k {
                0 => LocalKind::This,
                _ => LocalKind::Param,
            };
            g.new_local(&l.name, l.ty, l.mutable, span, kind);
        }
        let r = g.new_local("#result", gives, false, span, LocalKind::Temp);
        for s in &mut stmts {
            let StmtKind::Return(Some(call)) = &mut s.kind else {
                continue;
            };
            let hole = hir::Expr {
                kind: H::Lit(hir::Lit::Unit),
                ty: gives,
                span,
            };
            let init = std::mem::replace(call, hole);
            let mode = g.use_mode(gives, Want::Move);
            let read = g.mk(H::Local(r, mode), gives, span);
            let value = match g.try_coerce(read, want) {
                Ok(v) | Err(v) => v,
            };
            *call = hir::Expr {
                kind: H::Block(hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: StmtKind::Let {
                            local: r,
                            init: Some(init),
                        },
                        span,
                    }],
                    value: Some(Box::new(value)),
                    span,
                }),
                ty: want,
                span,
            };
        }
        g.cx.checking.pop();
        let frame = std::mem::replace(&mut g.f, Frame::new(FnKind::Method, None));
        drop(g);
        if let Some(Def::Fn(f)) = &mut self.defs[def.0 as usize] {
            f.body.locals = frame.locals;
            f.body.block.stmts = stmts;
        }
    }
}

/// Is `e` the local `l` itself, or an `Upcast` of it (the same object)?
fn same_object(e: &hir::Expr, l: LocalId) -> bool {
    match &e.kind {
        H::Local(x, _) => *x == l,
        H::Upcast(inner) => same_object(inner, l),
        _ => false,
    }
}
