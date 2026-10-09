//! Calling a value whose type is a union of function types with the same parameters
//! (`h: (() => void) | (() => Promise<void>)`, then `h()`), as TS allows. The call is a `match`
//! on the callee with one arm per member, each calling it with the checked arguments: only one
//! arm runs, so the arguments are evaluated once, after the callee, as in JS. The result is the
//! union of the members' results; `void` cannot be a union member, so a `void` member makes the
//! result nullable (`Promise<void> | null`; `await` on it gives `void`).

use velt_common::Span;
use velt_syntax::ast;

use super::super::LocalKind;
use crate::body::FnCx;
use super::args::Callable;
use crate::defs::ParamSig;
use crate::hir::{self, Callee, ExprKind as H, PassMode, PatKind as P, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `f(args)` when `f` is a union of function types with equal parameter lists; `Err(f)`
    /// for any other callee.
    pub(super) fn call_fn_union(
        &mut self,
        f: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((def, _)) = self.cx.union_def(f.ty) else {
            return Err(f);
        };
        let Some(members) = self.cx.union_members(f.ty) else {
            return Err(f);
        };
        let mut sigs = vec![];
        for m in &members {
            match self.cx.ty.kind(*m).clone() {
                TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                } => sigs.push((params, ret, throws)),
                _ => return Err(f),
            }
        }
        if sigs.iter().any(|(ps, _, _)| *ps != sigs[0].0) {
            return Err(f);
        }
        let params: Vec<ParamSig> = sigs[0]
            .0
            .iter()
            .enumerate()
            .map(|(i, ty)| ParamSig {
                name: format!("arg{i}"),
                span,
                ty: *ty,
                mode: if self.cx.is_copy(*ty) {
                    PassMode::Copy
                } else {
                    PassMode::Borrow
                },
                default: None,
            })
            .collect();
        let unit = self.cx.ty.unit;
        let never = self.cx.ty.never;
        let values: Vec<TyId> = sigs
            .iter()
            .map(|(_, r, _)| *r)
            .filter(|r| *r != unit && *r != never)
            .collect();
        let has_void = sigs.iter().any(|(_, r, _)| *r == unit);
        let ret = if values.is_empty() {
            if has_void { unit } else { never }
        } else {
            self.cx.union_of(&values, has_void, span)
        };
        let c = Callable {
            what: "this function".into(),
            params,
            ret,
            slot_names: vec![],
            bounds: vec![],
            js_numbers: false,
            js_api: false,
            rest: false,
            defaults: vec![],
        };
        let ck = self.check_call(&c, vec![], args, None, span);
        for (_, _, throws) in &sigs {
            if *throws != never {
                self.throw_src(crate::defs::ThrowSrc::Direct(*throws, span));
            }
        }
        let mut arms = vec![];
        for (v, (m, (_, r, _))) in members.iter().zip(&sigs).enumerate() {
            let b = self.new_local("<fn>", *m, false, span, LocalKind::Bind);
            let pat = self.pat(P::Binding(b, UseMode::Borrow), *m, span);
            let pat = self.pat(
                P::Variant {
                    def,
                    variant: v as u32,
                    args: vec![pat],
                },
                f.ty,
                span,
            );
            let callee = self.mk(H::Local(b, UseMode::Borrow), *m, span);
            let call = self.mk(
                H::Call {
                    callee: Callee::Indirect(Box::new(callee)),
                    args: ck.args.clone(),
                },
                *r,
                span,
            );
            let body = if ret == unit || ret == never {
                call
            } else if *r == unit {
                // A `void` member: the call, then `null`.
                let stmt = hir::Stmt {
                    kind: hir::StmtKind::Expr(call),
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
                self.coerce(call, ret)
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let mut f = f;
        crate::body::places::set_place_mode(&mut f, UseMode::Borrow);
        let kind = H::Match {
            scrutinee: Box::new(f),
            arms,
        };
        Ok(self.mk(kind, ret, span))
    }
}
