//! Gated functions (#1330): a std function whose first statement is
//! `__intrinsic_target("sha-x86")` is compiled for that CPU extension (backends may use it
//! anywhere in the function), so it must only run where the CPU has it. Sema checks every call:
//! it sits in the `then` branch of `if (u32x4.hasShaX86())` (velt:simd's method, resolved by
//! definition), or in a function gated on the same target. `f.call(this, …)` and
//! `f.apply(this, [...])` are direct calls of `f` and checked as such. A gated function cannot
//! be used as a value, and a closure written inside one starts without its target
//! (`Frame::target`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::defs::FnSource;
use crate::hir::{self, CpuTarget, DefId};

/// The target name a statement `__intrinsic_target("…")` gives.
fn target_call(stmt: &ast::Stmt) -> Option<&str> {
    let ast::StmtKind::Expr(e) = &stmt.kind else {
        return None;
    };
    let ast::ExprKind::Call { callee, args, .. } = &e.kind else {
        return None;
    };
    let ast::ExprKind::Ident(id) = &callee.kind else {
        return None;
    };
    if id.name != "__intrinsic_target" {
        return None;
    }
    match args.as_slice() {
        [arg] => match &arg.kind {
            ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.as_str()),
            _ => None,
        },
        _ => None,
    }
}

impl FnCx<'_, '_> {
    /// The CPU extension the condition `c` checks: a call of velt:simd's `u32x4.hasShaArm()`,
    /// `u32x4.hasShaX86()`, `u64x2.hasClmulX86()` or `u64x2.hasClmulArm()`, resolved to its
    /// definition (so also through `import { u32x4 as V }`,
    /// and never a user type or function that is only named like it).
    pub(super) fn cpu_guard(&self, c: &hir::Expr) -> Option<CpuTarget> {
        let hir::ExprKind::Call {
            callee: hir::Callee::Def(d, _),
            args,
        } = &c.kind
        else {
            return None;
        };
        let f = self.cx.try_fn(*d)?;
        if !args.is_empty() || !self.cx.scopes[f.module].is_std {
            return None;
        }
        CpuTarget::ALL.into_iter().find(|t| f.name == t.check())
    }

    /// The target function `d` is gated on: its first statement names it (std functions only).
    pub(crate) fn fn_target(&self, d: DefId) -> Option<CpuTarget> {
        let f = self.cx.fn_info(d);
        if !self.cx.scopes[f.module].is_std {
            return None;
        }
        let Some(FnSource::Decl(decl)) = &f.source else {
            return None;
        };
        CpuTarget::named(target_call(decl.body.stmts.first()?)?)
    }

    /// The statement `__intrinsic_target("sha-x86")`: allowed only as the first statement of a
    /// function declared in std (not a method or closure). Records the function as gated.
    pub(super) fn target_stmt(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let named = match args {
            [arg] => match &arg.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => CpuTarget::named(s),
                _ => None,
            },
            _ => None,
        };
        let Some(target) = named else {
            let names: Vec<&str> = CpuTarget::ALL.iter().map(|t| t.name()).collect();
            let message = format!(
                "`__intrinsic_target` takes the target as a string literal: one of {}",
                names.join(", ")
            );
            self.cx.err(message, span);
            return self.unit_expr(span);
        };
        let gated = self
            .body_def
            .filter(|&d| self.outer.is_empty() && self.cx.fn_info(d).this.is_none())
            .filter(|&d| self.fn_target(d) == Some(target));
        let Some(d) = gated else {
            self.cx.err(
                "`__intrinsic_target` must be the first statement of a function (not a method or closure)",
                span,
            );
            return self.unit_expr(span);
        };
        self.cx.cpu_targets.insert(d, target);
        self.f.target = Some(target);
        self.unit_expr(span)
    }

    /// A call of function `d` (named `name`): a gated one must be behind its target's check.
    pub(super) fn check_gated_call(&mut self, d: DefId, name: &str, span: Span) {
        let Some(target) = self.fn_target(d) else {
            return;
        };
        if self.f.target == Some(target) || self.f.cpu_guards.contains(&target) {
            return;
        }
        let d = Diagnostic::error(
            format!(
                "`{name}` runs only where the CPU has `{}`, so it must be called behind that check",
                target.name()
            ),
            span,
        )
        .with_note(format!(
            "call it inside `if ({}()) {{ … }}`, or from a function gated on the same target (a closure does not count)",
            target.check_call()
        ));
        self.cx.error(d);
    }

    /// Function `d` (named `name`) used as a value: an error when it is gated. Whether it is.
    pub(super) fn gated_value(&mut self, d: DefId, name: &str, span: Span) -> bool {
        let Some(target) = self.fn_target(d) else {
            return false;
        };
        let d = Diagnostic::error(
            format!(
                "`{name}` is compiled for `{}` and can only be called directly",
                target.name()
            ),
            span,
        )
        .with_note(format!(
            "a gated function cannot be stored, passed or returned: call it inside `if ({}()) {{ … }}`",
            target.check_call()
        ));
        self.cx.error(d);
        true
    }
}
