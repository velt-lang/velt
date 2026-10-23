//! Shared state (docs/reference/async.md): `new Mutex<T>(x)` and `m.with((v) => ...)`
//! on a `Mutex<T>` or `shared<Mutex<T>>`, and `new RwLock<T>(x)` with `rw.write(f)` (the same as
//! `with`) and `rw.read(f)` (`Intrinsic::LockRead`: the callback may not modify the value,
//! checked once modes are inferred, `ownership/locked`). The callback runs synchronously under the
//! lock and gets the value itself (lowering passes it by reference, so even a Copy parameter
//! written by the callback updates it); it cannot be async. The atomics on `shared<int>`
//! (`add`/`get`/`set`) are plain builtin methods.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, GenCopy, Want};
use crate::ctx::Item;
use crate::hir::{self, Def, DefId, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `new Mutex<T>(x)` or `new RwLock<T>(x)` when the class is the prelude's; `None` for any
    /// other `new`.
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
        let item = self.lookup_item(&name.name, name.span);
        let (new, what) = match item {
            Some(Item::Def(d)) if Some(d) == self.cx.mutex_ty() => (Intrinsic::MutexNew, "Mutex"),
            Some(Item::Def(d)) if Some(d) == self.cx.rwlock_ty() => {
                (Intrinsic::RwLockNew, "RwLock")
            }
            _ => return None,
        };
        let c = self.intrinsic_sig(new, span);
        let mut slots = vec![None];
        match targs.as_slice() {
            [] => {}
            [t] => slots[0] = Some(self.resolve(t)),
            _ => self
                .cx
                .err(format!("`{what}` takes 1 type argument"), class.span),
        }
        let ck = self.check_call(&c, slots, args, self.hint(exp), span);
        Some(self.intrinsic(new, ck.args, ck.ret, span))
    }

    /// `m.with(f)` / `rw.write(f)` (`read`: `rw.read(f)`): lock, `f(value)`, unlock; the result
    /// is `f`'s.
    pub(super) fn mutex_with(
        &mut self,
        mut recv: hir::Expr,
        args: &[ast::Expr],
        span: Span,
        read: bool,
    ) -> hir::Expr {
        let value = self
            .cx
            .mutex_value(recv.ty)
            .expect("ICE: `with` resolved on a non-mutex");
        let name = match (read, self.cx.is_rwlock(recv.ty)) {
            (true, _) => "read",
            (false, true) => "write",
            (false, false) => "with",
        };
        let [arg] = args else {
            self.arg_count_error(&format!("method `{name}`"), 1, 1, args.len(), span);
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let unknown = self.cx.ty.error;
        let expected = self.cx.ty.fn_ptr(vec![value], unknown);
        let f = match as_arrow(arg) {
            Some(a) => self.with_callback(a, expected, name),
            None => self.expr(arg, Some(expected), Want::Borrow),
        };
        let ret = match self.cx.ty.kind(f.ty).clone() {
            TyKind::FnPtr { params, ret, .. } if params == [value] => ret,
            TyKind::Error => return self.error_expr(span),
            _ => {
                let (found, v) = (self.cx.display(f.ty), self.cx.display(value));
                self.cx.error(
                    Diagnostic::error(
                        format!("`{name}` needs a function `(value: {v}) => R`, found `{found}`"),
                        f.span,
                    )
                    .with_note("the function gets the locked value and runs under the lock"),
                );
                return self.error_expr(span);
            }
        };
        if self.cx.holds_weak(ret) {
            self.no_weak_copy(ret, GenCopy::LockResult, f.span);
        }
        set_place_mode(&mut recv, UseMode::Borrow);
        let lock = match read {
            true => Intrinsic::LockRead,
            false => Intrinsic::MutexWith,
        };
        self.intrinsic(lock, vec![recv, f], ret, span)
    }

    /// The arrow passed to `with`: synchronous; lowering passes the locked value by reference,
    /// so assigning the parameter (even of a Copy type) updates it.
    fn with_callback(&mut self, arrow: &ast::Expr, expected: TyId, name: &str) -> hir::Expr {
        let ast::ExprKind::Arrow { is_async, .. } = &arrow.kind else {
            unreachable!("ICE: as_arrow")
        };
        if *is_async {
            self.cx.error(
                Diagnostic::error(
                    format!("the function passed to `{name}` cannot be async"),
                    arrow.span,
                )
                .with_note(format!(
                    "it runs while the lock is held; await before or after `{name}`"
                )),
            );
        }
        let f = self.closure(arrow, Some(expected), false);
        if let H::Closure(def) = f.kind {
            self.no_weak_in_lock(def);
        }
        f
    }

    /// A `with` callback cannot use a weak collection from outside the lock: a part of the
    /// locked value it keys (or targets) would be released, or read, by another thread without
    /// the lock (docs/internals/design/weak-refs.md "Threads and `shared`"). Reports each capture
    /// of closure `def` that holds one.
    pub(crate) fn no_weak_in_lock(&mut self, def: DefId) {
        let caps: Vec<(TyId, String, Span)> = match &self.cx.defs[def.0 as usize] {
            Some(Def::Fn(f)) => f
                .captures
                .iter()
                .map(|c| &f.body.locals[c.inner.0 as usize])
                .map(|l| (l.ty, l.name.clone(), l.span))
                .collect(),
            _ => return,
        };
        for (ty, name, at) in caps {
            if self.cx.holds_weak(ty) {
                self.no_weak_copy(ty, GenCopy::LockCapture(name), at);
            }
        }
    }
}
