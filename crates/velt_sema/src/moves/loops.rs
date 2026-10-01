//! Loops in the moves dataflow: iterate silently to a fixpoint, then run one reporting pass.
//! `break` / `continue` states are collected per loop and joined at the exit / back edge.

use super::state::{join, Flow};
use super::Moves;
use crate::hir::{Block, Expr, ExprKind, Lit, Pat, Stmt, StmtKind};

pub(super) struct LoopFlow {
    label: Option<String>,
    breaks: Flow,
    conts: Flow,
    /// `Moves::open` blocks when the loop body started: a jump leaves the ones after.
    depth: usize,
}

/// The parts of a `While` / `ForOf` the dataflow needs.
struct LoopParts<'a> {
    label: Option<&'a String>,
    cond: Option<&'a Expr>,
    /// `while (true)`: the loop is only left by `break`.
    always: bool,
    /// `for...of` binding, (re)initialized at the top of every iteration.
    binding: Option<&'a Pat>,
    body: &'a Block,
    step: Option<&'a Expr>,
}

impl Moves<'_> {
    pub(super) fn loop_stmt(&mut self, s: &Stmt, st: &mut Flow) {
        let parts = match &s.kind {
            StmtKind::While {
                label,
                cond,
                body,
                step,
            } => LoopParts {
                label: label.as_ref(),
                cond: Some(cond),
                always: matches!(cond.kind, ExprKind::Lit(Lit::Bool(true))),
                binding: None,
                body,
                step: step.as_ref(),
            },
            StmtKind::ForOf {
                label,
                binding,
                iter,
                body,
                ..
            } => {
                self.expr(iter, st);
                LoopParts {
                    label: label.as_ref(),
                    cond: None,
                    always: false,
                    binding: Some(binding),
                    body,
                    step: None,
                }
            }
            _ => unreachable!("ICE: loop_stmt on a non-loop"),
        };
        let entry = st.take();
        let saved = self.report;
        self.report = false;
        let mut head = entry.clone();
        for _ in 0..256 {
            let (_, back) = self.loop_iter(&parts, head.clone());
            let next = join(entry.clone(), back);
            if next == head {
                break;
            }
            head = next;
        }
        self.report = saved;
        let (exit, _) = self.loop_iter(&parts, head);
        *st = exit;
    }

    /// One pass over the loop from `head`; returns (state after the loop, state at the back edge).
    fn loop_iter(&mut self, p: &LoopParts, head: Flow) -> (Flow, Flow) {
        let mut s = head;
        if let Some(c) = p.cond {
            self.expr(c, &mut s);
        }
        let cond_exit = if p.always { None } else { s.clone() };
        if let Some(b) = p.binding {
            Self::init_pat(b, &mut s);
        }
        self.loops.push(LoopFlow {
            label: p.label.cloned(),
            breaks: None,
            conts: None,
            depth: self.open.len(),
        });
        self.block(p.body, &mut s);
        let conts = self.loops.last_mut().expect("ICE: loop stack").conts.take();
        let mut s = join(s, conts);
        if let Some(step) = p.step {
            let saved = std::mem::replace(&mut self.in_step, true);
            self.expr(step, &mut s);
            self.in_step = saved;
        }
        let lf = self.loops.pop().expect("ICE: loop stack");
        (join(cond_exit, lf.breaks), s)
    }

    /// `break` (`is_break`) or `continue` to the labelled / innermost loop.
    pub(super) fn jump(&mut self, label: Option<&str>, is_break: bool, st: &mut Flow) {
        let target = match label {
            None => self.loops.len().checked_sub(1),
            Some(l) => self
                .loops
                .iter()
                .rposition(|lp| lp.label.as_deref() == Some(l)),
        };
        if let Some(i) = target {
            let mut f = st.take();
            // The jump leaves the blocks opened inside the loop: closures held there are gone.
            if let Some(s) = &mut f {
                for block in &self.open[self.loops[i].depth..] {
                    block.iter().for_each(|l| s.drop_holder(*l));
                }
            }
            let lp = &mut self.loops[i];
            if is_break {
                lp.breaks = join(lp.breaks.take(), f);
            } else {
                lp.conts = join(lp.conts.take(), f);
            }
        }
        *st = None;
    }
}
