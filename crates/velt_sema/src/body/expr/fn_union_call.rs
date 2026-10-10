//! Calling a value whose type is a union of function types with the same parameters
//! (`h: (() => void) | (() => Promise<void>)`, then `h()`), as TS allows, or where each
//! member's parameters begin the longest member's (`((a: number) => string) | (() => string)`
//! called as `f(1)`): the call takes the longest member's arguments, and a shorter member
//! ignores the extra ones. The call is a `match`
//! on the callee with one arm per member, each calling it with the checked arguments: only one
//! arm runs, so the arguments are evaluated once, after the callee, as in JS. The result is the
//! union of the members' results; `void` cannot be a union member, so a `void` member makes the
//! result nullable (`Promise<void> | null`; `await` on it gives `void`).

use velt_common::Span;
use velt_syntax::ast;

use super::super::LocalKind;
use super::args::Callable;
use crate::body::places::{is_path, is_place, set_place_mode};
use crate::body::FnCx;
use crate::defs::ParamSig;
use crate::hir::{
    self, Callee, ExprKind as H, Intrinsic, PassMode, PatKind as P, TyId, TyKind, UseMode,
};

impl FnCx<'_, '_> {
    /// `f(args)` when `f` is a union of function types whose parameter lists are prefixes of
    /// the longest one; `Err(f)` for any other callee.
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
        // The longest parameter list; every other member's must be a prefix of it.
        let longest = sigs
            .iter()
            .map(|(ps, _, _)| ps.clone())
            .max_by_key(|ps| ps.len())
            .unwrap_or_default();
        if sigs.iter().any(|(ps, _, _)| !longest.starts_with(ps)) {
            return Err(f);
        }
        let params: Vec<ParamSig> = longest
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
            if has_void {
                unit
            } else {
                never
            }
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
        let mut ck = self.check_call(&c, vec![], args, None, span);
        // A member taking fewer parameters ignores the extra arguments, which JS evaluates
        // anyway, in order: when one is left out, every argument that is a value rather than
        // a variable or literal is evaluated into a temporary first, and so is every variable
        // or field before one (which may assign it: `g(a, (a = 2))` passes the old `a`).
        let mut temps = vec![];
        if sigs.iter().any(|(ps, _, _)| ps.len() < longest.len()) {
            let last = ck.args.iter().rposition(|a| !evaluated_anywhere(a));
            for (k, a) in ck.args.iter_mut().enumerate() {
                let earlier = last.is_some_and(|l| k < l);
                if earlier || !evaluated_anywhere(a) {
                    self.arg_temp(a, &mut temps);
                }
            }
        }
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
            let n = match self.cx.ty.kind(*m) {
                TyKind::FnPtr { params, .. } => params.len(),
                _ => ck.args.len(),
            };
            let call = self.mk(
                H::Call {
                    callee: Callee::Indirect(Box::new(callee)),
                    args: ck.args[..n].to_vec(),
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
        let call = self.mk(kind, ret, span);
        Ok(self.with_temps_value(temps, call))
    }

    /// Replaces the argument `a` by a temporary holding it (a `let` appended to `temps`), unless
    /// it is a literal or an arrow; a place's value is shared (or copied).
    fn arg_temp(&mut self, a: &mut hir::Expr, temps: &mut Vec<hir::Stmt>) {
        if matches!(a.kind, H::Lit(_) | H::Closure(_)) {
            return;
        }
        let (ty, span) = (a.ty, a.span);
        if is_place(a) && !self.cx.is_copy(ty) {
            set_place_mode(a, UseMode::Borrow);
            let place = std::mem::replace(a, self.error_expr(span));
            *a = self.intrinsic(Intrinsic::Share, vec![place], ty, span);
        }
        let tmp = self.new_local("<arg>", ty, false, span, LocalKind::Temp);
        let mode = if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        };
        let init = std::mem::replace(a, self.mk(H::Local(tmp, mode), ty, span));
        temps.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(init),
            },
            span,
        });
    }
}

/// An argument evaluated without effects, which a later argument may change: a variable or
/// field path, a literal or an arrow.
fn evaluated_anywhere(a: &hir::Expr) -> bool {
    is_path(a) || matches!(a.kind, H::Lit(_) | H::Closure(_))
}
