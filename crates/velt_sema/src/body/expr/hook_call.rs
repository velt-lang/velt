//! `hookCall("role", args…)` (std/lang/intrinsics/hook_call.velt): std code calling a function hook
//! of velt_common's `std_hooks.rs` by its role, without importing its module. The call demands
//! the module (`Ctx::demand_std`), as #2161 (a1) loads a module the check finds needed, and is
//! then an ordinary call of the hook's definition, checked against its declaration: the result is
//! a plain call, so lowering knows nothing of it. A prelude method can so reach code that is
//! loaded only by the programs that call the method (#2198); `@hook` (#2203 step 3) can replace
//! it without a change of behaviour.

use velt_common::hook_sig::Sig;
use velt_common::std_hooks::HOOKS;
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// `hookCall(role, args…)`: the call of the hook `role` with `args`, or an error.
    pub(super) fn hook_call(
        &mut self,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some((first, rest)) = args.split_first() else {
            return self.hook_call_error("`hookCall` needs the hook's role first", span, args);
        };
        let ast::ExprKind::Lit(ast::Lit::Str(role)) = &first.kind else {
            return self.hook_call_error(
                "the role of `hookCall` must be a string literal",
                first.span,
                args,
            );
        };
        let Some(h) = HOOKS.iter().find(|h| h.name == role.as_str()) else {
            let msg = format!("`{role}` is no hook of velt_common's `std_hooks.rs`");
            return self.hook_call_error(&msg, first.span, args);
        };
        if !matches!(h.sig, Sig::Fn(_)) {
            let msg = format!("the hook `{role}` is not a function");
            return self.hook_call_error(&msg, first.span, args);
        }
        self.cx.demand_std(&h.module());
        let Some(Item::Def(d)) = self.cx.std_def(&h.qualified()) else {
            let msg = format!("the hook `{role}` ({}) is not loaded", h.qualified());
            return self.hook_call_error(&msg, first.span, args);
        };
        let name = h.item.to_string();
        self.fn_call(d, &name, type_args, rest, exp, span)
    }

    /// Reports `msg` at `span` and checks `args` loosely; an error expression.
    fn hook_call_error(&mut self, msg: &str, span: Span, args: &[ast::Expr]) -> hir::Expr {
        self.cx.error(Diagnostic::error(msg, span));
        self.check_args_loose(args);
        self.error_expr(span)
    }
}
