//! `perf/cpu-check-in-loop`: a CPU check (velt/arch's `Avx2.isSupported()`, or one of velt/simd's
//! older methods such as `u64x2.hasClmulX86()`) inside a loop. Its answer never changes while the
//! program runs, but where the build doesn't decide it at compile time it is a load and a test of
//! the start-up feature word each time: in a hot loop that costs (#3198 measured 22% on a bf16 dot
//! product whose check was read every iteration). Dispatch once, outside the loop: check, then run
//! a function gated on the feature, or the portable one, whose loop has no check
//! (docs/internals/design/package-machine-code.md §2 "Checks inside loops"). It costs nothing in a
//! debug build, where every check is the constant `false`, which is why a lint says so.
//!
//! The checks are recognised by their definitions (in velt/arch, or velt/simd's and std's older
//! check methods), never by name alone. A loop's body, its condition and a `for` loop's update
//! count; a check in a function defined inside the loop does too, as it usually runs per
//! iteration. Each check is reported once, however many loops it sits in.

use std::collections::HashSet;

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind, StmtKind};
use velt_syntax::visit::{walk_block, walk_expr, walk_module, Visit};

use super::Cx;
use crate::Finding;

pub(crate) const CODE: &str = "perf/cpu-check-in-loop";

/// velt/simd's and std's check methods from before velt/arch, by the name a call writes.
const OLDER: &[&str] = &[
    "hasShaArm",
    "hasShaX86",
    "hasClmulX86",
    "hasClmulArm",
    "hasClmul",
    "hasAesX86",
    "hasAesArm",
    "hasAes",
    "hasBf16Arm",
    "hasFmaX86",
];

/// The std folders whose definitions are CPU checks.
const CHECK_MODULES: &[&str] = &[
    "std/arch/x86/",
    "std/arch/arm/",
    "std/simd/",
    "std/lang/math/fma_check",
];

pub(super) fn find(cx: &Cx) -> Vec<Finding> {
    struct Loops<'c, 'a> {
        cx: &'c Cx<'a>,
        seen: HashSet<Span>,
        out: Vec<Finding>,
    }
    impl<'a> Visit<'a> for Loops<'_, '_> {
        fn stmt(&mut self, s: &'a ast::Stmt) {
            let mut checks = Checks {
                cx: self.cx,
                found: vec![],
            };
            match &s.kind {
                StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                    walk_expr(cond, &mut checks);
                    walk_block(body, &mut checks);
                }
                StmtKind::For {
                    cond, update, body, ..
                } => {
                    for e in cond.iter().chain(update) {
                        walk_expr(e, &mut checks);
                    }
                    walk_block(body, &mut checks);
                }
                StmtKind::ForOf { body, .. } => walk_block(body, &mut checks),
                _ => return,
            }
            // A check in an inner loop is found by the outer loop's walk too: once is enough.
            for (span, call) in checks.found {
                if self.seen.insert(span) {
                    self.out.push(finding(span, call));
                }
            }
        }
    }
    let mut v = Loops {
        cx,
        seen: HashSet::new(),
        out: vec![],
    };
    walk_module(&cx.module.ast, &mut v);
    v.out
}

/// The CPU checks called in what it walks: each call's span and its text.
struct Checks<'c, 'a> {
    cx: &'c Cx<'a>,
    found: Vec<(Span, String)>,
}

impl<'a> Visit<'a> for Checks<'_, '_> {
    fn expr(&mut self, e: &'a ast::Expr) {
        let ExprKind::Call { callee, args, .. } = &e.kind else {
            return;
        };
        let ExprKind::Member { prop, .. } = &callee.kind else {
            return;
        };
        if !args.is_empty() || !(prop.name == "isSupported" || OLDER.contains(&prop.name.as_str()))
        {
            return;
        }
        if is_check(self.cx, prop.span) {
            self.found.push((e.span, self.cx.text(e.span).to_string()));
        }
    }
}

/// Whether the member named at `span` is one of std's CPU checks.
fn is_check(cx: &Cx, span: Span) -> bool {
    let Some(def) = cx.analysis.def_of(span) else {
        return false;
    };
    let Some(module) = cx.modules.get(def.module) else {
        return false;
    };
    module.is_std && CHECK_MODULES.iter().any(|m| module.path.starts_with(m))
}

fn finding(span: Span, call: String) -> Finding {
    Finding {
        code: CODE,
        span,
        message: format!("`{call}` is checked in a loop"),
        notes: vec![
            "the answer never changes while the program runs, but where the build doesn't decide \
             it, each check loads and tests the CPU's feature word"
                .to_string(),
            "check once before the loop, and run a function gated on the feature \
             (`@targetFeature`) or the portable one"
                .to_string(),
        ],
        fixes: vec![],
    }
}
