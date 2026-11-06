//! CPU checks known while lowering (#2456). Where a build cannot have an extension, its check
//! `velt_simd_has_<ext>()` is false: on another architecture, and under a code generator without
//! the instructions (`LowerOptions::no_cpu_extensions`), as the backends answer it. A call of a
//! function that only combines such checks (`u32x4.hasShaArm()`, `arm()` in std/simd/aes.velt) is
//! known too. `const_cond` takes these, and lowering is on demand, so a gated function or an
//! emulation reached only from a branch known false is never built. A function also called from
//! elsewhere is built for that call.
//!
//! Only "false because the build cannot have it" is decided here. A check that may be true is
//! the CPU's to answer at run time, or `velt_opt`'s and the LLVM backend's in a function gated on
//! the extension (`gated_clones`, `own_extension_check`).

use rustc_hash::FxHashMap;
use velt_sema::hir::{self, DefId};

use super::Cx;
use crate::simd::SimdCall;

/// What `Cx` knows about CPU checks.
#[derive(Default)]
pub(super) struct CpuChecks {
    /// `LowerOptions::no_cpu_extensions`.
    pub(super) no_extensions: bool,
    /// The answer per function without parameters that lowering asked about. `None` is not a
    /// known check; it is also the answer while a function is being looked at, so that calls in
    /// a cycle are no check.
    known: FxHashMap<DefId, Option<bool>>,
}

/// The architecture of a target triple, as `Ext::on_arch` names it; `None` for one this code does
/// not know, whose checks stay at run time (`native`, `host`).
fn arch_of(triple: &str) -> Option<&str> {
    match triple.split('-').next()? {
        arch @ ("x86_64" | "aarch64" | "wasm32") => Some(arch),
        "arm64" => Some("aarch64"),
        _ => None,
    }
}

impl Cx<'_> {
    /// The value of a call of `d` without arguments, when it is a CPU check this build answers.
    pub(super) fn cpu_check(&mut self, d: DefId) -> Option<bool> {
        if let Some(&known) = self.cpu_checks.known.get(&d) {
            return known;
        }
        self.cpu_checks.known.insert(d, None);
        let hir = self.hir;
        let value = match hir.def(d) {
            hir::Def::ExternFn(x) => self.extension_absent(&x.symbol).then_some(false),
            hir::Def::Fn(f) if is_plain_call(f) => {
                returned(&f.body.block).and_then(|e| self.cpu_check_expr(e))
            }
            _ => None,
        };
        self.cpu_checks.known.insert(d, value);
        value
    }

    /// `e`, a function's whole result, when CPU checks decide it. Evaluation order is kept: an
    /// operand that decides it comes first, so nothing with an effect is skipped.
    fn cpu_check_expr(&mut self, e: &hir::Expr) -> Option<bool> {
        match &e.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Def(d, targs),
                args,
            } if args.is_empty() && targs.is_empty() => self.cpu_check(*d),
            hir::ExprKind::Unary {
                op: hir::UnOp::Not,
                expr,
            } => self.cpu_check_expr(expr).map(|v| !v),
            hir::ExprKind::Logical { op, lhs, rhs } => {
                let and = *op == hir::LogicOp::And;
                match self.cpu_check_expr(lhs)? {
                    l if l != and => Some(l),
                    _ => self.cpu_check_expr(rhs),
                }
            }
            _ => None,
        }
    }

    /// Whether `symbol` is the check of an extension this build cannot have.
    fn extension_absent(&self, symbol: &str) -> bool {
        let Some(SimdCall::Has(ext)) = SimdCall::of_symbol(symbol) else {
            return false;
        };
        self.cpu_checks.no_extensions || arch_of(&self.target).is_some_and(|a| !ext.on_arch(a))
    }
}

/// Whether a call of `f` without arguments runs its body and gives its result.
fn is_plain_call(f: &hir::FnDef) -> bool {
    f.params.is_empty()
        && f.captures.is_empty()
        && f.generics == 0
        && !f.is_async
        && !f.is_generator
}

/// The expression a body consisting of one `return` gives.
fn returned(block: &hir::Block) -> Option<&hir::Expr> {
    match (block.stmts.as_slice(), &block.value) {
        ([], Some(value)) => Some(value),
        (
            [hir::Stmt {
                kind: hir::StmtKind::Return(Some(e)),
                ..
            }],
            None,
        ) => Some(e),
        _ => None,
    }
}
