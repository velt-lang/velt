//! Debug builds' inlining: only the compiler's small helpers and std's trivial forwarders are
//! inlined (`run_helpers`, `std_forwarders`), so user code keeps its calls and stays debuggable.

use velt_vir::vir::{Callee, Function, Inline, Program, Terminator};

use super::{cost, never_returns, splice, TRIVIAL_COST};
use crate::callgraph::direct_calls;

/// Inline every direct call to a function marked in `helpers` (debug builds: the compiler's
/// small helpers only, so user code keeps its calls and stays debuggable). Helpers must not
/// call each other.
/// [`run_helpers`] into function `fi` alone, the helpers' bodies from `bodies` (as they were
/// when they were lowered: `velt dev`'s kept debug pipeline, lib.rs).
pub(crate) fn run_helpers_into(
    program: &mut Program,
    fi: usize,
    helpers: &[bool],
    bodies: &rustc_hash::FxHashMap<usize, velt_vir::vir::Function>,
) {
    let original_blocks = program.funcs[fi].blocks.len();
    for bi in 0..original_blocks {
        let Terminator::Call {
            callee: Callee::Func(callee),
            args,
            ..
        } = &program.funcs[fi].blocks[bi].term
        else {
            continue;
        };
        let ci = callee.0 as usize;
        let Some(body) = bodies
            .get(&ci)
            .filter(|_| helpers.get(ci).copied().unwrap_or(false))
        else {
            continue;
        };
        if body.blocks.is_empty() || !splice::arity_matches(args, body) {
            continue;
        }
        let resume = splice::resume_extern(&program.externs);
        splice::inline_call(&program.aggs, &mut program.funcs[fi], bi, body, resume);
    }
}

pub(crate) fn run_helpers(program: &mut Program, helpers: &[bool]) -> bool {
    let mut changed = false;
    for fi in 0..program.funcs.len() {
        if helpers[fi] {
            continue;
        }
        let original_blocks = program.funcs[fi].blocks.len();
        for bi in 0..original_blocks {
            let Terminator::Call {
                callee: Callee::Func(callee),
                args,
                ..
            } = &program.funcs[fi].blocks[bi].term
            else {
                continue;
            };
            let ci = callee.0 as usize;
            // A declaration (an `Import` without a body) stays a call.
            if !helpers.get(ci).copied().unwrap_or(false)
                || program.funcs[ci].blocks.is_empty()
                || !splice::arity_matches(args, &program.funcs[ci])
            {
                continue;
            }
            let body = program.funcs[ci].clone();
            let resume = splice::resume_extern(&program.externs);
            splice::inline_call(&program.aggs, &mut program.funcs[fi], bi, &body, resume);
            changed = true;
        }
    }
    changed
}

/// Add to `helpers` (debug builds, [`run_helpers`]) the standard library's trivial functions
/// (cost at most `TRIVIAL_COST`: a forwarder such as `appendJsonString(out, s)` or a builder's
/// one-call method, and the forwarders generated code for std writes): each is a function the
/// backend would compile, for a call that stepping through user code never enters. Those that
/// call a helper stay functions (helpers do not call each other), and so do std/lang's (the
/// compiler's hooks and the language's runtime code).
pub(crate) fn std_forwarders(funcs: &[Function], helpers: &mut [bool]) {
    let candidate: Vec<bool> = (funcs.iter().enumerate())
        .map(|(i, f)| {
            // Not std/lang: the compiler's hooks and the language's runtime code, whose calls
            // stay visible in a debug build's code. Except the helpers of the code generated
            // for `deepEqual` and `toEqual` (std/lang/equal), which only that code calls.
            let lang = f.symbol.starts_with("_V3stdP4lang") && !f.symbol.starts_with(STD_EQUAL);
            let std =
                (f.symbol.starts_with("_V3std") && !lang) || f.symbol.starts_with(GENERATED_STD);
            std && !helpers[i]
                && !f.blocks.is_empty()
                // Exported too: the std cache's objects may call it (its body stays).
                && f.linkage != velt_vir::vir::Linkage::Import
                && !never_returns(f)
                && f.target.is_empty()
                && f.inline != Inline::Never
                && cost(f) <= TRIVIAL_COST
        })
        .collect();
    for (i, f) in funcs.iter().enumerate() {
        let calls_helper = direct_calls(f).any(|c| {
            let c = c.0 as usize;
            c == i || candidate.get(c) == Some(&true) || helpers.get(c) == Some(&true)
        });
        if candidate[i] && !calls_helper {
            helpers[i] = true;
        }
    }
}

/// The symbol prefix of std/lang/equal: `generated.velt`'s helpers (`identity`, `beginPair`, …).
const STD_EQUAL: &str = "_V3stdP4langP5equal";

/// The symbol prefix of the code generated for std's generated functions
/// (`<generated>/std/…`, velt_sema `generated`).
pub(super) const GENERATED_STD: &str = "_V15_3cgenerated_3eP3std";
