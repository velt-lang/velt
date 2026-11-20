//! Gated functions (#1330): a function decorated `@targetFeature("sha-x86")` (std/lang/intrinsics/
//! decorators.velt, re-exported by velt/compiler; any module, docs/internals/design/
//! package-machine-code.md §2) is compiled for that CPU extension (backends may use it anywhere
//! in the function), so it must only run where the CPU has it. Sema checks every call: it sits in
//! the `then` branch of `if (u32x4.hasShaX86())` (velt/simd's method, resolved by definition), or
//! in a function gated on the same target. `f.call(this, …)` and `f.apply(this, [...])` are direct
//! calls of `f` and checked as such. A gated function cannot be used as a value, and a closure
//! written inside one starts without its target (`Frame::target`). The decorator's type keeps it
//! off methods (and closures take no decorators), and [`FnCx::ungateable`] refuses the functions
//! nothing would check before running them (the program's `main`, one C calls) and those whose
//! body is compiled into another function that the target would not reach (a nested, async or
//! generator function), or that their overload signatures call unchecked.

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

    /// The use of `@targetFeature` on function `d` (found by its role, never by the name it is
    /// imported under), if any.
    fn target_use(&self, d: DefId) -> Option<&hir::Decorator> {
        let uses = self.cx.decorator_uses.get(&DecoratorTarget::Def(d))?;
        let Some(Item::Def(role)) = self.cx.std_def(std_hook!(decorator_target_feature)) else {
            return None;
        };
        uses.iter().find(|u| u.def == role)
    }

    /// The target function `d` is gated on: the argument of its `@targetFeature`. Read at every
    /// call, so a function without decorators costs one map lookup. A function the decorator
    /// can't gate ([`FnCx::ungateable`], reported at its body) is not gated.
    pub(crate) fn fn_target(&self, d: DefId) -> Option<CpuTarget> {
        let u = self.target_use(d)?;
        if self.cx.fn_info(d).kind != FnKind::Free || self.ungateable(d).is_some() {
            return None;
        }
        match u.args.first() {
            Some(ConstTree::Str(t)) => CpuTarget::named(t),
            _ => None,
        }
    }

    /// Why `@targetFeature` can't gate function `d`, as (what it is, a note), when it can't.
    /// Nothing checks the CPU before the runtime calls `main` or C calls an `@export` function;
    /// a nested function is compiled as a closure, and an async function or a generator into a
    /// function that resumes it, neither of which lowering gives the target
    /// (`velt_vir` `lower/abi.rs` reads `cpu_targets` by the declaration only); and an overload
    /// signature's body calls the implementation without the check.
    fn ungateable(&self, d: DefId) -> Option<(&'static str, &'static str)> {
        let f = self.cx.fn_info(d);
        if f.module == self.cx.root && f.name == "main" {
            return Some((
                "the program's entry, `main`",
                "the runtime calls it without checking the CPU: gate a function `main` calls behind the check",
            ));
        }
        let exported = (self.cx.std_def(std_hook!(decorator_export)))
            .zip(self.cx.decorator_uses.get(&DecoratorTarget::Def(d)))
            .is_some_and(|(e, uses)| uses.iter().any(|u| Some(Item::Def(u.def)) == Some(e)));
        if exported {
            return Some((
                "a function C calls (`@export`)",
                "C calls it without checking the CPU: export a function that calls the gated one behind the check",
            ));
        }
        if f.is_async || f.is_async_gen || f.is_generator {
            return Some((
                "an async function or a generator",
                "its body is compiled into a function that resumes it, which the target would not reach: gate a plain function it calls",
            ));
        }
        if self.cx.nested_locals.contains_key(&d) {
            return Some((
                "a function declared inside another",
                "it is compiled as a closure, which the target would not reach: declare it at the top level of its module",
            ));
        }
        let overloaded = crate::overloads::sig_of(&f.name).is_some()
            || (self.cx.scopes[f.module].items)
                .contains_key(&crate::overloads::sig_name(&f.name, 0));
        if overloaded {
            return Some((
                "an overloaded function",
                "its signatures call the implementation without checking the CPU: gate a plain function the implementation calls behind the check",
            ));
        }
        None
    }

    /// The body of function `d` is being checked: a gated one records its target
    /// (`hir::Program::cpu_targets`) and may call that target's gated functions; the decorator on
    /// a function it can't gate is an error.
    pub(super) fn gated_body(&mut self, d: DefId) {
        if let Some(target) = self.fn_target(d) {
            self.cx.cpu_targets.insert(d, target);
            self.f.target = Some(target);
            return;
        }
        let Some(span) = self.target_use(d).map(|u| u.span) else {
            return;
        };
        if let Some((what, note)) = self.ungateable(d) {
            self.cx.error(
                Diagnostic::error(format!("`@targetFeature` can't gate {what}"), span)
                    .with_note(note),
            );
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
