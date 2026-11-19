//! Gated functions (#1330): a function decorated `@targetFeature("sha-x86")` (std/lang/intrinsics/
//! decorators.velt, re-exported by velt/compiler; any module, docs/internals/design/
//! package-machine-code.md §2) is compiled for that CPU extension (backends may use it anywhere
//! in the function), so it must only run where the CPU has it. Sema checks every call: it sits in
//! the `then` branch of `if (u32x4.hasShaX86())` (velt/simd's method, resolved by definition), or
//! in a function gated on the same target. `f.call(this, …)` and `f.apply(this, [...])` are direct
//! calls of `f` and checked as such. A gated function cannot be used as a value, and a closure
//! written inside one starts without its target (`Frame::target`). The decorator's type keeps it
//! off methods (and closures take no decorators); a function declared inside another may be
//! gated, its calls checked like any other's.

use velt_common::{std_hook, Diagnostic, Span};

use super::FnCx;
use crate::ctx::Item;
use crate::defs::FnKind;
use crate::hir::{self, ConstTree, CpuTarget, DecoratorTarget, DefId};

impl FnCx<'_, '_> {
    /// The CPU extension the condition `c` checks: a call of velt/simd's `u32x4.hasShaArm()`,
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

    /// The target function `d` is gated on: the argument of its `@targetFeature` (found by its
    /// role, never by the name it is imported under). Read at every call, so a function without
    /// decorators costs one map lookup.
    pub(crate) fn fn_target(&self, d: DefId) -> Option<CpuTarget> {
        let uses = self.cx.decorator_uses.get(&DecoratorTarget::Def(d))?;
        let Some(Item::Def(role)) = self.cx.std_def(std_hook!(decorator_target_feature)) else {
            return None;
        };
        let u = uses.iter().find(|u| u.def == role)?;
        if self.cx.fn_info(d).kind != FnKind::Free {
            return None;
        }
        match u.args.first() {
            Some(ConstTree::Str(t)) => CpuTarget::named(t),
            _ => None,
        }
    }

    /// The body of function `d` is being checked: a gated one records its target
    /// (`hir::Program::cpu_targets`) and may call that target's gated functions.
    pub(super) fn gated_body(&mut self, d: DefId) {
        if let Some(target) = self.fn_target(d) {
            self.cx.cpu_targets.insert(d, target);
            self.f.target = Some(target);
        }
    }

    /// A call of function `d` (named `name`): a gated one must be behind its target's check.
    pub(super) fn check_gated_call(&mut self, d: DefId, name: &str, span: Span) {
        let Some(target) = self.fn_target(d) else {
            return;
        };
        let covered = |t: &CpuTarget| t.covers(target);
        if self.f.target.as_ref().is_some_and(covered) || self.f.cpu_guards.iter().any(covered) {
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
