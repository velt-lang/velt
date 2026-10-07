//! `velt build --report numbers`: the `f64` locals inside loops that `numrep` leaves as
//! doubles, each with the first reason it found (the static counterpart of V8's deopt traces).

use velt_vir::vir::{Function, SrcLoc, Stmt, Ty};

use super::flow::Flow;
use super::narrow::{self, Reason};
use super::Env;

/// A source-level `number` variable inside a loop that stays a double.
#[derive(Clone, Debug, PartialEq)]
pub struct Unnarrowed {
    /// The variable's name.
    pub name: String,
    /// Where it is first assigned in the loop, when known.
    pub loc: Option<SrcLoc>,
    /// Why it is not an integer, in words.
    pub reason: String,
}

/// The named `f64` locals of `func` assigned inside a loop that stay `f64`.
pub(super) fn unnarrowed(env: &Env, flow: &Flow, func: &Function) -> Vec<Unnarrowed> {
    let plan = narrow::plan(env, flow, func);
    let edges: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| {
            crate::visit::successors(&b.term)
                .iter()
                .map(|s| s.0 as usize)
                .collect()
        })
        .collect();
    let in_loop = crate::scc::on_cycle(&edges);
    let mut out: Vec<Unnarrowed> = vec![];
    let mut seen = vec![false; func.locals.len()];
    for (bi, block) in func.blocks.iter().enumerate() {
        if !in_loop[bi] {
            continue;
        }
        for (si, s) in block.stmts.iter().enumerate() {
            let Stmt::Assign(d, _) = s else { continue };
            let i = d.local.0 as usize;
            let decl = &func.locals[i];
            let Some(name) = decl.name.as_ref() else {
                continue;
            };
            let free = plan.reasons[i] == Some(Reason::NoGain);
            if !d.proj.is_empty() || decl.ty != Ty::F64 || plan.to[i].is_some() || seen[i] || free {
                continue;
            }
            seen[i] = true;
            let reason = match plan.reasons[i] {
                Some(r) => describe(r, func),
                None => "it is a parameter, a call result or stored in memory".to_string(),
            };
            out.push(Unnarrowed {
                name: name.clone(),
                loc: func.loc(bi, si),
                reason,
            });
        }
    }
    out
}

/// The words for each reason; `docs/tooling/cli.md` (`--report numbers`) lists the same ones.
fn describe(r: Reason, func: &Function) -> String {
    match r {
        Reason::NotWhole => "it may hold a fraction".into(),
        Reason::MayBeNan => "it may be NaN".into(),
        Reason::Unbounded(lo, hi) => {
            format!("it may reach ±2^53 (values in [{lo}, {hi}]): no loop bound or `| 0` limits it")
        }
        Reason::Form => "it is set from a division, a call or memory".into(),
        Reason::NegZero(b, s) => match func.loc(b, s) {
            Some(l) => format!("it may be -0, which line {} can tell from 0", l.line),
            None => "it may be -0, which a use can tell from 0".into(),
        },
        Reason::Wide => "it is set from a value that is not a 32-bit integer".into(),
        Reason::NoGain => "it is only converted from an integer and used as a double".into(),
    }
}
