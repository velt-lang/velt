//! Gated functions (#1330): a function decorated `@targetFeature("sha-x86")` (std/lang/intrinsics/
//! decorators.velt, re-exported by velt/compiler; any module, docs/internals/design/
//! package-machine-code.md §2) is compiled for that CPU extension (backends may use it anywhere
//! in the function), so it must only run where the CPU has it. Sema checks every call: it sits in
//! the `then` branch of `if (Sha.isSupported())` (velt/arch's checks, or velt/simd's older methods
//! such as `u32x4.hasShaX86()`, resolved by definition), or in a function gated on features that
//! cover its target. `f.call(this, …)` and `f.apply(this, [...])` are direct
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
use crate::hir::{self, ConstTree, CpuTarget, CpuTargets, DecoratorTarget, DefId};

impl FnCx<'_, '_> {
    /// The CPU features the condition `c` checks: checks joined by `&&` (`Avx2.isSupported() &&
    /// Bmi2.isSupported()`), each a call of velt/arch's `isSupported()` or one of velt/simd's older
    /// methods ([`FnCx::cpu_check_of`]). Another operand of the `&&` (`n > 64 && Avx2.isSupported()`)
    /// adds nothing; a condition with no check is `None`.
    pub(super) fn cpu_guard(&self, c: &hir::Expr) -> Option<CpuTargets> {
        let set = self.cpu_checks_in(c, CpuTargets::NONE);
        (!set.is_empty()).then_some(set)
    }

    fn cpu_checks_in(&self, c: &hir::Expr, set: CpuTargets) -> CpuTargets {
        match &c.kind {
            hir::ExprKind::Logical {
                op: hir::LogicOp::And,
                lhs,
                rhs,
            } => self.cpu_checks_in(rhs, self.cpu_checks_in(lhs, set)),
            _ => self.cpu_check_of(c).map_or(set, |t| set.with(t)),
        }
    }

    /// The CPU feature the call `c` checks: velt/arch's `Avx2.isSupported()` (any class of
    /// `velt/arch/x86` or `velt/arch/arm`), or one of velt/simd's older methods
    /// (`u32x4.hasShaArm()`, `u64x2.hasClmulX86()`, …), resolved to its definition (so also
    /// through `import { Avx2 as A }`, and never a user type or function only named like it).
    fn cpu_check_of(&self, c: &hir::Expr) -> Option<CpuTarget> {
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
        hir::arch_check(&f.name).or_else(|| {
            (CpuTarget::ALL.iter().copied()).find(|t| hir::cpu_check(*t) == Some(f.name.as_str()))
        })
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

    /// The features function `d` is gated on: the arguments of its `@targetFeature`. Read at
    /// every call, so a function without decorators costs one map lookup. A function the
    /// decorator can't gate ([`FnCx::ungateable`], reported at its body) is not gated.
    pub(crate) fn fn_target(&self, d: DefId) -> Option<CpuTargets> {
        if let Some(t) = self.arch_instruction(d) {
            return Some(CpuTargets::NONE.with(t));
        }
        let u = self.target_use(d)?;
        if self.cx.fn_info(d).kind != FnKind::Free || self.ungateable(d).is_some() {
            return None;
        }
        let names = match u.args.first() {
            Some(ConstTree::Array(items)) => items.as_slice(),
            _ => return None,
        };
        let set = (names.iter()).fold(CpuTargets::NONE, |set, n| match n {
            ConstTree::Str(t) => CpuTarget::named(t).map_or(set, |t| set.with(t)),
            _ => set,
        });
        (!set.is_empty()).then_some(set)
    }

    /// The feature velt/arch's instruction `d` needs (#3222 step 4): a static method of a class of
    /// `velt/arch/x86` or `velt/arch/arm` other than `isSupported` (`Sse42.crc32U64`) is gated
    /// on that class's feature, as if it had its `@targetFeature`.
    fn arch_instruction(&self, d: DefId) -> Option<CpuTarget> {
        let f = self.cx.try_fn(d)?;
        if f.kind != FnKind::Static || !self.cx.scopes[f.module].is_std {
            return None;
        }
        let (module, item) = f.name.rsplit_once("::")?;
        let (class, method) = item.split_once('.')?;
        if method == "isSupported" {
            return None;
        }
        hir::arch_check(&format!("{module}::{class}.isSupported"))
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
            // Features of both architectures: no CPU has them all, so the function never runs
            // (each call needs every check), which is a mistake rather than a dispatch.
            let mut arches = target.iter().map(|t| t.arch());
            let first = arches.next();
            if arches.any(|a| Some(a) != first) {
                let span = self.target_use(d).map_or_else(Span::default, |u| u.span);
                self.cx.error(
                    Diagnostic::error(
                        "`@targetFeature` names features of both x86-64 and arm64".to_string(),
                        span,
                    )
                    .with_note("no CPU has them all, so the function could never run: gate one function per architecture, each behind its own checks"),
                );
            }
            return;
        }
        let Some(u) = self.target_use(d) else {
            return;
        };
        let span = u.span;
        // `@targetFeature()`: the rest parameter allows no name, and no feature gates nothing.
        if matches!(u.args.first(), Some(ConstTree::Array(items)) if items.is_empty())
            && self.ungateable(d).is_none()
        {
            self.cx.error(
                Diagnostic::error("`@targetFeature` names no feature".to_string(), span)
                    .with_note("name the features the function is compiled for: `@targetFeature(\"avx2-x86\")`"),
            );
            return;
        }
        if let Some((what, note)) = self.ungateable(d) {
            self.cx.error(
                Diagnostic::error(format!("`@targetFeature` can't gate {what}"), span)
                    .with_note(note),
            );
        }
    }

    /// A call of function `d` (named `name`): a gated one must be behind checks of its features,
    /// or in a function gated on a set that covers them: every feature of its set is covered by
    /// the caller's set together with the checks around the call.
    pub(super) fn check_gated_call(&mut self, d: DefId, name: &str, span: Span) {
        let Some(target) = self.fn_target(d) else {
            return;
        };
        let have = (self.f.cpu_guards.iter().flat_map(|g| g.iter()))
            .fold(self.f.target.unwrap_or_default(), |set, g| set.with(g));
        if target.iter().all(|t| have.covers(t)) {
            return;
        }
        let (names, checks) = described(target);
        let d = Diagnostic::error(
            format!("`{name}` runs only where the CPU has {names}, so it must be called behind {}", if target.iter().count() == 1 { "that check" } else { "those checks" }),
            span,
        )
        .with_note(format!(
            "call it inside {checks}, or from a function gated on the same features (a closure does not count)"
        ));
        self.cx.error(d);
    }

    /// Function `d` (named `name`) used as a value: an error when it is gated. Whether it is.
    pub(super) fn gated_value(&mut self, d: DefId, name: &str, span: Span) -> bool {
        let Some(target) = self.fn_target(d) else {
            return false;
        };
        let (names, checks) = described(target);
        let d = Diagnostic::error(
            format!("`{name}` is compiled for {names} and can only be called directly"),
            span,
        )
        .with_note(format!(
            "a gated function cannot be stored, passed or returned: call it inside {checks}"
        ));
        self.cx.error(d);
        true
    }
}

/// A gated set for messages: its names (`` `avx2-x86` and `bmi2-x86` ``) and the checks that
/// cover it, joined by `&&`.
fn described(set: CpuTargets) -> (String, String) {
    let names: Vec<String> = set.iter().map(|t| format!("`{}`", t.name())).collect();
    let checks: Vec<String> = set.iter().map(hir::cpu_check_call).collect();
    let mut modules: Vec<&str> = set.iter().map(|t| t.arch_module()).collect();
    modules.dedup();
    let checks = format!(
        "`if ({}) {{ … }}` (the {} from {})",
        checks.join(" && "),
        if checks.len() == 1 { "check" } else { "checks" },
        modules.join(" and ")
    );
    (names.join(" and "), checks)
}
