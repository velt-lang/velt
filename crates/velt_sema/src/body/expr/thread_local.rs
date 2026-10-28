//! `ThreadLocal<T>` (std/prelude/sync.vlt, docs/std/sync.md): one value per thread, of a number
//! type or `bool`. `new ThreadLocal<T>(literal)` is the struct holding the starting value, and a
//! call of `get` or `set` is a `ThreadLocal*` intrinsic on the module constant holding it, which
//! lowering names by its `DefId` (velt_vir `lower/thread_local.rs`). Whether the receiver is such
//! a constant is checked once every constant's initializer is known (`crate::thread_locals`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, Conversion, DefId, ExprKind as H, Intrinsic, TyId, TyKind, UnOp};

impl FnCx<'_, '_> {
    /// `new ThreadLocal<T>(x)` once `mutex_new` found the class is the prelude's.
    pub(super) fn thread_local_new(
        &mut self,
        d: DefId,
        class: &ast::TypeExpr,
        targs: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        // `(x: T) -> ThreadLocal<T>`: `ThreadLocalSet`'s value parameter, and its receiver type.
        let mut c = self.intrinsic_sig(Intrinsic::ThreadLocalSet, span);
        c.what = "`ThreadLocal` constructor".into();
        c.ret = c.params[0].ty;
        c.params = vec![c.params[1].clone()];
        let mut slots = vec![None];
        match targs {
            [] => {}
            [t] => slots[0] = Some(self.resolve(t)),
            _ => self
                .cx
                .err("`ThreadLocal` takes 1 type argument", class.span),
        }
        let ck = self.check_call(&c, slots, args, self.hint(exp), span);
        let t = ck.type_args.first().copied().unwrap_or(self.cx.ty.error);
        let ok = self.thread_local_value_ty(t, class.span);
        if ok && !ck.args.first().is_some_and(is_literal) {
            let at = ck.args.first().map_or(span, |a| a.span);
            self.cx.error(
                Diagnostic::error("a `ThreadLocal`'s starting value must be a literal", at)
                    .with_note("every thread starts with it, so the compiler stores it with the program: write a number or `true`/`false`"),
            );
        }
        let kind = H::AdtLit {
            def: d,
            type_args: vec![t],
            fields: ck.args,
            converted: Conversion::No,
        };
        self.mk(kind, ck.ret, span)
    }

    /// Reports a `T` a `ThreadLocal<T>` cannot hold; whether it can.
    fn thread_local_value_ty(&mut self, t: TyId, span: Span) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Error => true,
            _ => {
                let shown = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error(
                        format!("`ThreadLocal<T>` needs a number type or `bool`, found `{shown}`"),
                        span,
                    )
                    .with_note("a thread's value is never dropped, so it holds no object: keep objects in a structure indexed by a per-thread number"),
                );
                false
            }
        }
    }

    /// The call of a method `def` of `ThreadLocal` or `Atomic` as its intrinsic
    /// (`thread_local_call`, `atomic_call`); `None` for any other method.
    pub(super) fn sync_call(
        &mut self,
        def: DefId,
        args: &mut Vec<hir::Expr>,
        ret: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        self.thread_local_call(def, args, ret, span)
            .or_else(|| self.atomic_call(def, args, ret, span))
    }

    /// The call of `ThreadLocal`'s method `def` with the checked arguments `args` (the receiver
    /// first) as its intrinsic; `None` when `def` is not one of its methods. Inside the
    /// declarations, the calls stay calls.
    fn thread_local_call(
        &mut self,
        def: DefId,
        args: &mut Vec<hir::Expr>,
        ret: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let info = self.cx.fn_info(def);
        let owner = self.cx.thread_local_ty();
        if info.owner.is_none() || info.owner != owner {
            return None;
        }
        let name = info.name.rsplit('.').next().unwrap_or("").to_string();
        let i = match name.as_str() {
            "get" => Intrinsic::ThreadLocalGet,
            "set" => Intrinsic::ThreadLocalSet,
            _ => return None,
        };
        let recv = args.first()?;
        let t = self.cx.thread_local_value(recv.ty)?;
        if !self.thread_local_value_ty(t, span) {
            return Some(self.error_expr(span));
        }
        let global = match &recv.kind {
            H::Global(d) => Some(*d),
            _ => None,
        };
        self.cx.thread_local_uses.push((global, recv.span));
        let out: Vec<hir::Expr> = std::mem::take(args);
        Some(self.intrinsic(i, out, ret, span))
    }
}

/// Is `e` a literal (a negative number is `-` applied to one)?
/// A literal, or a negated number literal.
pub(crate) fn is_literal(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Lit(_) => true,
        H::Unary {
            op: UnOp::Neg,
            expr,
        } => matches!(expr.kind, H::Lit(_)),
        _ => false,
    }
}
