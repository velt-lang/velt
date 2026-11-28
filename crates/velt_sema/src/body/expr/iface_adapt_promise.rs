//! A promise converted inside an interface adapter (`iface_adapt`): a method's result
//! `Promise<Res<S>>` read through the interface at `S = unknown` is a promise of the converted
//! value (#379, sigx's `validate(): Result<O> | Promise<Result<O>>`). What it rejects with
//! stays; only the value it settles with is converted, when it settles.

use velt_syntax::ast;

use super::callback::synth;
use super::iface_adapt::{value_ok, Mode};
use super::iface_adapt_slots::fields_ok;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Ctx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

/// Does a result of union type `from` convert to union `to` inside an adapter (`value_ok`)?
/// Member by member, each with one of the other's that takes it: a union holding another
/// (`Res<O> | Promise<Res<O>>` with `Res` a union) is flattened, and the order of its members
/// differs between its instances, so they are not paired by position. A promise member is a
/// promise of the converted value (`validate(): Promise<Res<S>>` read at `S = unknown`): what
/// it rejects with stays, and only the settled value is converted.
pub(super) fn union_result_ok(cx: &mut Ctx, from: TyId, to: TyId, m: &mut Mode) -> bool {
    let (Some(xs), Some(ys)) = (cx.union_members(from), cx.union_members(to)) else {
        return false;
    };
    xs.len() == ys.len()
        && xs.iter().all(|a| {
            ys.iter()
                .any(|b| a == b || value_ok(cx, *a, *b, false, m) || fields_ok(cx, *a, *b, m))
        })
}

/// Does a promise's settled value of type `a` convert to `b` inside an adapter (`value_ok`'s
/// promise rule)? Not a promise of a promise: [`FnCx::adapt_promise`] converts the value the
/// promise settles with, in the `then` callback, and a promise there is not converted again.
pub(super) fn settled_ok(cx: &mut Ctx, a: TyId, b: TyId, m: &mut Mode) -> bool {
    let nested = [a, b]
        .iter()
        .any(|t| matches!(cx.ty.kind(*t), TyKind::Promise(..)));
    !nested && value_ok(cx, a, b, false, m)
}

impl FnCx<'_, '_> {
    /// Promise `h` as the promise type `exp` of another settled type, inside an adapter:
    /// `{ const src = h; const f = (v) => v; src.then(f) }`, the arrow checked against
    /// `(v: A) => B`, which converts the value. `Err` for other types, and when the two reject
    /// with different types.
    pub(super) fn adapt_promise(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let (TyKind::Promise(a, x), TyKind::Promise(b, y)) =
            (self.cx.ty.kind(h.ty).clone(), self.cx.ty.kind(exp).clone())
        else {
            return Err(h);
        };
        if x != y || a == b {
            return Err(h);
        }
        let span = h.span;
        let never = self.cx.ty.never;
        let fn_ty = self.cx.ty.intern(TyKind::FnPtr {
            params: vec![a],
            ret: b,
            throws: never,
            rest: false,
        });
        let n = self.f.locals.len();
        let ident = |name: String| ast::Ident { name, span };
        let (src_name, f_name, v_name) = (
            ident(format!("#adaptsrc{n}")),
            ident(format!("#adaptf{n}")),
            ident(format!("#adaptv{n}")),
        );
        let var = |id: &ast::Ident| synth(ast::ExprKind::Ident(id.clone()), span);
        self.push_scope();
        let src = self.declare_local(&src_name, h.ty, LocalKind::Const);
        let arrow = synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params: vec![ast::ArrowParam {
                    name: v_name.clone(),
                    pattern: None,
                    ty: None,
                    default: None,
                    optional: false,
                    rest: false,
                }],
                ret: None,
                throws: None,
                body: ast::ArrowBody::Expr(Box::new(var(&v_name))),
                is_async: false,
            },
            span,
        );
        let w = self.closure(&arrow, Some(fn_ty), true);
        let f = self.declare_local(&f_name, w.ty, LocalKind::Const);
        let then = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(var(&src_name)),
                        prop: ident(super::promise_then::CONVERT_THEN.into()),
                        optional: false,
                    },
                    span,
                )),
                type_args: vec![],
                args: vec![var(&f_name)],
                optional: false,
            },
            span,
        );
        let call = self.expr(&then, Some(exp), Want::Move);
        if call.ty != exp {
            self.pop_scope();
            return Err(self.error_expr(span));
        }
        // It stands for the method's promise, as `promise_retype`'s `then` does (`PromiseSame`,
        // #1682): handled where this one is. Without it, `Promise.all` handling this promise
        // left the method's own to be reported as an uncaught rejection.
        let held = self.expr(&var(&src_name), None, Want::Move);
        let callee = hir::Callee::Intrinsic(hir::Intrinsic::PromiseSame);
        let args = vec![call, held];
        let call = self.mk(H::Call { callee, args }, exp, span);
        self.pop_scope();
        let mut h = h;
        self.force_move(&mut h);
        let stmt = |local, init| hir::Stmt {
            kind: hir::StmtKind::Let {
                local,
                init: Some(init),
            },
            span,
        };
        let block = hir::Block {
            stmts: vec![stmt(src, h), stmt(f, w)],
            value: Some(Box::new(call)),
            span,
        };
        Ok(self.mk(H::Block(block), exp, span))
    }
}
