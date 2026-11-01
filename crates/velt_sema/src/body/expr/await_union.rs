//! `await x` where `x` is a union with promise members (`Resp | Promise<Resp, E>`, the result of
//! a sync-or-async callback): as in JS a promise member is awaited and any other value is the
//! result as it is. The result type is the union of the promises' payloads and the other
//! members (`void` payloads and `null` together give `void`). Lowered as a `match` on `x`.

use velt_common::Span;

use super::super::LocalKind;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, PatKind as P, UseMode};

impl FnCx<'_, '_> {
    /// The awaited value of `h`, or `Err(h)` when `h` is not a union with a promise member;
    /// `on_promise` gives the value of an arm whose member is a promise (`read`, settling with
    /// the payload type given): `Await(read)`, or a never-typed expression for a promise an async
    /// function adopts (`tasks.rs`).
    pub(super) fn await_union_with(
        &mut self,
        h: hir::Expr,
        span: Span,
        on_promise: &mut dyn FnMut(&mut Self, hir::Expr, hir::TyId) -> hir::Expr,
    ) -> Result<hir::Expr, hir::Expr> {
        let sty = h.ty;
        if self.is_three_state(sty) {
            return Err(h);
        }
        let inner = self.cx.ty.core(sty).unwrap_or(sty);
        // `Promise<T> | null` is an optional promise, not a union: one member, no variant.
        let (def, members) = match self.cx.union_def(inner) {
            Some((def, _)) => match self.cx.union_members(inner) {
                Some(ms) => (Some(def), ms),
                None => return Err(h),
            },
            None if inner != sty && self.cx.ty.promise_payload(inner).is_some() => {
                (None, vec![inner])
            }
            None => return Err(h),
        };
        if !members
            .iter()
            .any(|m| self.cx.ty.promise_payload(*m).is_some())
        {
            return Err(h);
        }
        let unit = self.cx.ty.unit;
        let never = self.cx.ty.never;
        let mut nullable = inner != sty;
        // The result's nothing: `undefined` for an optional promise read as `undefined` (a
        // `Record` or `Map` lookup) and for a `void` payload, as in Node.
        let mut has_null = self.cx.ty.nothing_of(sty).is_some_and(|n| n.has_null());
        let mut values = vec![];
        for m in &members {
            let v = self.cx.ty.promise_payload(*m).unwrap_or(*m);
            if self.cx.ty.nothing_of(v).is_some_and(|n| n.has_null()) {
                has_null = true;
            }
            if v == unit {
                nullable = true;
            } else if v != never {
                values.push(v);
            }
            if let Some(e) = self.cx.ty.promise_error(*m) {
                if e != never {
                    self.throw_src(crate::defs::ThrowSrc::Direct(e, span));
                }
            }
        }
        let ret = if values.is_empty() {
            unit
        } else {
            let t = self.cx.union_of(&values, nullable, span);
            match *self.cx.ty.kind(t) {
                hir::TyKind::Option(p, hir::Nothing::Null) if !has_null => {
                    self.cx.ty.option_of(p, hir::Nothing::Undefined)
                }
                _ => t,
            }
        };
        let unit_expr = |s: &mut Self| {
            let block = hir::Block {
                stmts: vec![],
                value: None,
                span,
            };
            s.mk(H::Block(block), unit, span)
        };
        let mut arms = vec![];
        if inner != sty {
            let body = if ret == unit {
                unit_expr(self)
            } else {
                self.mk(H::Lit(hir::Lit::Null), ret, span)
            };
            arms.push(hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body,
            });
        }
        for (v, m) in members.iter().enumerate() {
            let mode = if self.cx.is_copy(*m) {
                UseMode::Copy
            } else {
                UseMode::Move
            };
            let b = self.new_local("<awaited>", *m, false, span, LocalKind::Bind);
            let bind = self.pat(P::Binding(b, mode), *m, span);
            let mut pat = match def {
                Some(def) => self.pat(
                    P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![bind],
                    },
                    inner,
                    span,
                ),
                None => bind,
            };
            if inner != sty {
                pat = self.pat(P::Some(Box::new(pat)), sty, span);
            }
            let read = self.mk(H::Local(b, mode), *m, span);
            let (value, vty) = match self.cx.ty.promise_payload(*m) {
                Some(p) => {
                    let value = on_promise(self, read, p);
                    let vty = value.ty;
                    (value, vty)
                }
                None => (read, *m),
            };
            let body = if ret == unit {
                if vty == unit {
                    value
                } else {
                    let stmt = hir::Stmt {
                        kind: hir::StmtKind::Expr(value),
                        span,
                    };
                    let block = hir::Block {
                        stmts: vec![stmt],
                        value: None,
                        span,
                    };
                    self.mk(H::Block(block), unit, span)
                }
            } else if vty == unit {
                let stmt = hir::Stmt {
                    kind: hir::StmtKind::Expr(value),
                    span,
                };
                let null = self.mk(H::Lit(hir::Lit::Null), ret, span);
                let block = hir::Block {
                    stmts: vec![stmt],
                    value: Some(Box::new(null)),
                    span,
                };
                self.mk(H::Block(block), ret, span)
            } else {
                self.coerce(value, ret)
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, ret, span))
    }
}
