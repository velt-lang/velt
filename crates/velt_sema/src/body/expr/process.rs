//! Node's `process` members that cost nothing at run time, on the builtin `process` namespace:
//! `process.stdout.write(s)` and `process.stderr.write(s)` (a string, without a newline; `true`
//! like Node), and `process.env.NAME` / `process.env[name]` (`string | null`: there is no
//! `undefined`), and `process.argv` (Node's layout). They are calls of the prelude's
//! `__processWrite` / `__processEnv` / `__processArgv`
//! (std/prelude/process.vlt). `process.exit` is an intrinsic (`builtins`); the rest of Node's
//! `process` lives in `velt:process`.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId};

/// An expression the source doesn't contain (an argument filled in for the prelude call).
fn synthetic(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

/// The stream number `velt_rt_write_str` takes for `process.<name>`.
fn stream_of(name: &str) -> Option<i64> {
    match name {
        "stdout" => Some(1),
        "stderr" => Some(2),
        _ => None,
    }
}

impl FnCx<'_, '_> {
    /// Is `e` the builtin `process` (not a local or an item of that name)?
    fn is_builtin_process(&mut self, e: &ast::Expr) -> bool {
        let ast::ExprKind::Ident(id) = &e.kind else {
            return false;
        };
        id.name == "process"
            && !self.is_local_name(&id.name)
            && self.lookup_item(&id.name, id.span).is_none()
    }

    /// `process.<member>` where `object` is that member access: the member's name.
    fn process_member<'e>(&mut self, object: &'e ast::Expr) -> Option<&'e ast::Ident> {
        let ast::ExprKind::Member {
            object: base,
            prop,
            optional: false,
        } = &object.kind
        else {
            return None;
        };
        self.is_builtin_process(base).then_some(prop)
    }

    /// `process.stdout.write(s)` / `process.stderr.write(s)`; `None` for anything else.
    pub(super) fn process_write_call(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let stream = self.process_member(object)?;
        let fd = stream_of(&stream.name)?;
        if prop.name != "write" {
            let msg = format!("`process.{}` has no method `{}`", stream.name, prop.name);
            self.cx.error(
                velt_common::Diagnostic::error(msg, prop.span)
                    .with_note("use `write(s)`, or `velt:process` for byte writes"),
            );
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        }
        let fd = synthetic(
            ast::ExprKind::Lit(ast::Lit::Int {
                value: fd as u128,
                suffix: None,
            }),
            stream.span,
        );
        let mut full = vec![fd];
        full.extend(args.iter().cloned());
        let what = format!("process.{}.write", stream.name);
        Some(self.prelude_call("__processWrite", &what, &[], &full, exp, span))
    }

    /// `process.env.NAME`; `None` for anything else.
    pub(super) fn process_env_member(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.process_member(object)?.name != "env" {
            return None;
        }
        let name = synthetic(
            ast::ExprKind::Lit(ast::Lit::Str(prop.name.clone())),
            prop.span,
        );
        Some(self.prelude_call("__processEnv", "process.env", &[], &[name], exp, span))
    }

    /// `process.argv`: Node's `[runtime, script, ...args]`, a fresh array per read; `None` for
    /// anything else.
    pub(super) fn process_argv_member(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if prop.name != "argv" || !self.is_builtin_process(object) {
            return None;
        }
        Some(self.prelude_call("__processArgv", "process.argv", &[], &[], exp, span))
    }

    /// Is `target` `process.env.NAME` or `process.env[name]`?
    fn is_env_entry(&mut self, target: &ast::Expr) -> bool {
        let (ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. }) =
            &target.kind
        else {
            return false;
        };
        self.process_member(object).is_some_and(|m| m.name == "env")
    }

    /// `process.env.NAME = v` / `process.env[name] = v` (reported): environment variables are
    /// set with `setEnv` of `velt:process`.
    pub(super) fn reject_env_assign(&mut self, target: &ast::Expr) -> bool {
        if !self.is_env_entry(target) {
            return false;
        }
        self.cx.error(
            velt_common::Diagnostic::error("cannot assign to `process.env`", target.span)
                .with_note(
                    "use `setEnv(name, value)` from `velt:process` (not synchronized with reads \
                     on other threads: set variables at startup)",
                ),
        );
        true
    }

    /// `delete process.env.NAME` / `delete process.env[name]` (reported): environment variables
    /// are removed with `removeEnv` of `velt:process`.
    pub(super) fn reject_env_delete(&mut self, target: &ast::Expr) -> bool {
        if !self.is_env_entry(target) {
            return false;
        }
        self.cx.error(
            velt_common::Diagnostic::error("cannot `delete` from `process.env`", target.span)
                .with_note("use `removeEnv(name)` from `velt:process`"),
        );
        true
    }

    /// `process.env[name]`; `None` for anything else.
    pub(super) fn process_env_index(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.process_member(object)?.name != "env" {
            return None;
        }
        let args = std::slice::from_ref(index);
        Some(self.prelude_call("__processEnv", "process.env", &[], args, None, span))
    }
}
