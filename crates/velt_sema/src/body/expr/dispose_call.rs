//! Explicit `x[Symbol.dispose]()` on a value whose type has the drop hook: the value is
//! dropped right there (the hook runs, then its fields drop), so a resource can be released
//! before the end of its scope. The receiver is moved into a temporary of a block expression,
//! which drop elaboration then drops like any other local; the hook can never run twice.
//! Through an interface or a type parameter the call is rejected: the value's own drop would
//! run the hook a second time.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind};
use crate::collect::{lookup_method, Found, DISPOSE};
use crate::hir::{self, ExprKind as H, StmtKind as S, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `recv[Symbol.dispose](args)`.
    pub(super) fn explicit_dispose(
        &mut self,
        mut recv: hir::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        if !self.has_drop_hook(recv.ty) {
            let found = self.cx.display(recv.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`{found}` has no `[Symbol.dispose]()` drop hook to call"),
                    span,
                )
                .with_note("only a class or struct that declares `[Symbol.dispose]()` (or inherits it) can be disposed of explicitly"),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        if !args.is_empty() {
            self.cx.err("`[Symbol.dispose]()` takes no arguments", span);
            self.check_args_loose(args);
        }
        if let H::Local(l, _) = recv.kind {
            if self.local_kind(l) == LocalKind::Using {
                let name = self.f.locals[l.0 as usize].name.clone();
                self.cx.error(
                    Diagnostic::error(
                        format!("`{name}` is disposed at the end of its block: it is declared with `using`"),
                        span,
                    )
                    .with_note("declare it with `const` to dispose of it earlier"),
                );
                return self.error_expr(span);
            }
        }
        set_place_mode(&mut recv, UseMode::Move);
        let tmp = self.new_local("<disposed>", recv.ty, false, span, LocalKind::Temp);
        let stmt = hir::Stmt {
            kind: S::Let {
                local: tmp,
                init: Some(recv),
            },
            span,
        };
        let block = hir::Block {
            stmts: vec![stmt],
            value: None,
            span,
        };
        let unit = self.cx.ty.unit;
        self.mk(H::Block(block), unit, span)
    }

    /// Does a value of type `ty` have a `[Symbol.dispose]()` drop hook (declared by its class
    /// or an ancestor)?
    fn has_drop_hook(&mut self, ty: hir::TyId) -> bool {
        let TyKind::Adt(d, args) = self.cx.ty.kind(ty).clone() else {
            return false;
        };
        match lookup_method(self.cx, d, &args, DISPOSE) {
            Some(Found::Class { m, owner, .. }) if !m.is_static => {
                self.cx.adt(owner).is_some_and(|a| a.has_dispose)
            }
            _ => false,
        }
    }
}
