//! Shared state (docs/reference/async.md): `new Mutex<T>(x)` and `m.with((v) => ...)`
//! on a `Mutex<T>` or `shared<Mutex<T>>`. The callback runs synchronously under the lock and gets
//! the value itself (lowering passes it by reference, so even a Copy parameter written by the
//! callback updates it); it cannot be async. The atomics on
//! `shared<int>` (`add`/`get`/`set`) are plain builtin methods.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, Intrinsic, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `new Mutex<T>(x)` when `Mutex` is the prelude's; `None` for any other `new`.
    pub(super) fn mutex_new(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::TypeExprKind::Named { path, args: targs } = &class.kind else {
            return None;
        };
        let [name] = path.as_slice() else {
            return None;
        };
        let mutex = self.cx.mutex_ty()?;
        if self.lookup_item(&name.name, name.span) != Some(Item::Def(mutex)) {
            return None;
        }
        let c = self.intrinsic_sig(Intrinsic::MutexNew, span);
        let mut slots = vec![None];
        match targs.as_slice() {
            [] => {}
            [t] => slots[0] = Some(self.resolve(t)),
            _ => self.cx.err("`Mutex` takes 1 type argument", class.span),
        }
        let ck = self.check_call(&c, slots, args, self.hint(exp), span);
        Some(self.intrinsic(Intrinsic::MutexNew, ck.args, ck.ret, span))
    }

    /// `m.with(f)`: lock, `f(value)`, unlock; the result is `f`'s.
    pub(super) fn mutex_with(
        &mut self,
        mut recv: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let value = self
            .cx
            .mutex_value(recv.ty)
            .expect("ICE: `with` resolved on a non-mutex");
        let [arg] = args else {
            self.arg_count_error("method `with`", 1, 1, args.len(), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let unknown = self.cx.ty.error;
        let expected = self.cx.ty.fn_ptr(vec![value], unknown);
        let f = match as_arrow(arg) {
            Some(a) => self.with_callback(a, expected),
            None => self.expr(arg, Some(expected), Want::Borrow),
        };
        let ret = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { params, ret, .. } if params == [value] => ret,
            TyKind::Error => return self.error_expr(span),
            _ => {
                let (found, v) = (self.cx.display(f.ty), self.cx.display(value));
                self.cx.error(
                    Diagnostic::error(
                        format!("`with` needs a function `(value: {v}) => R`, found `{found}`"),
                        f.span,
                    )
                    .with_note("the function gets the locked value and runs under the lock"),
                );
                return self.error_expr(span);
            }
        };
        set_place_mode(&mut recv, UseMode::Borrow);
        self.intrinsic(Intrinsic::MutexWith, vec![recv, f], ret, span)
    }

    /// The arrow passed to `with`: synchronous; lowering passes the locked value by reference,
    /// so assigning the parameter (even of a Copy type) updates it.
    fn with_callback(&mut self, arrow: &ast::Expr, expected: TyId) -> hir::Expr {
        let ast::ExprKind::Arrow { is_async, .. } = &arrow.kind else {
            unreachable!("ICE: as_arrow")
        };
        if *is_async {
            self.cx.error(
                Diagnostic::error("the function passed to `with` cannot be async", arrow.span)
                    .with_note("it runs while the lock is held; await before or after `with`"),
            );
        }
        self.closure(arrow, Some(expected), false)
    }
}
