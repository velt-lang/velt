//! `switch` statements (docs/reference/control-flow.md "switch", hir_encodings.md "switch"): JS semantics —
//! the first case whose value equals the discriminant is entered (`default` when none does),
//! bodies fall through into the next case unless they `break` / return / throw.
//!
//! Desugaring (no HIR statement of its own):
//! - The dispatch is a `Match` on the scrutinee (`cases`): case values become patterns, so
//!   union / enum tags and integers dispatch through a jump table (VIR `Switch`).
//! - When no body falls through into another non-empty body (the common case), each group
//!   of labels with its body is one arm; `default` is the last arm (`_`).
//! - Otherwise the match yields the index of the entered case into a local, and the bodies
//!   follow as `if (<case> <= i) { body_i }` (fallthrough runs the later bodies too).
//! - When some `break` targets the `switch`, all of it is wrapped in a labelled
//!   `while (true) { ...; break; }` that the `break`s leave; a `continue` crossing the
//!   `switch` names its loop's label (synthesized when it has none).
//!
//! Case bodies are checked with the scrutinee local narrowed to the members that can reach
//! them (their own case, plus the cases falling through into them).

pub(crate) mod cases;
mod exhaustive;
mod select;

use velt_common::Span;
use velt_syntax::ast;

use self::cases::{Scrut, ScrutKind};
use self::select::Sel;
use super::narrow::Fact;
use super::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, PatKind as P, StmtKind as S};

/// A checked case: what it selects (`None`: `default`) and its body.
struct Case {
    sel: Option<Sel>,
    body: hir::Block,
    /// The body can complete normally (and so falls into the next case).
    falls: bool,
}

impl FnCx<'_, '_> {
    pub(crate) fn switch_stmt(
        &mut self,
        disc: &ast::Expr,
        cases: &[ast::SwitchCase],
        label: Option<&ast::Ident>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let mut stmts = vec![];
        let s = self.scrutinee(disc, &mut stmts);
        let sels: Vec<(Option<Sel>, Span)> = cases
            .iter()
            .map(|c| (c.test.as_ref().map(|t| self.case_sel(&s, t)), c.span))
            .collect();
        let has_default = cases.iter().any(|c| c.test.is_none());
        let missing = Self::uncovered(&s, &sels);
        let complete = has_default || (!s.slots.is_empty() && missing.is_empty());
        self.enter_loop(label, true);
        // Without `default`, the values no case takes skip the `switch` (TypeScript accepts a
        // switch that does not cover every member): after it they are what is left.
        let skipped = (!complete).then_some(missing);
        let checked = self.case_bodies(&s, cases, sels, skipped);
        let lp = self.exit_loop();
        stmts.extend(self.dispatch(s, checked, complete, span));
        let stmts = match (lp.has_break, lp.hir_label()) {
            (true, Some(l)) => vec![self.run_once(stmts, l, span)],
            _ => stmts,
        };
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        Self::push(out, S::Block(block), span);
    }

    /// Check every case body (from the narrowing state before the `switch`). Afterwards only
    /// what holds on every way out still holds: the bodies that leave the `switch` by `break`
    /// or by completing the last one, and the values no case takes (`skipped`, without
    /// `default`), which keep what holds before the `switch` narrowed to those members.
    fn case_bodies(
        &mut self,
        s: &Scrut,
        cases: &[ast::SwitchCase],
        sels: Vec<(Option<Sel>, Span)>,
        skipped: Option<Vec<usize>>,
    ) -> Vec<Case> {
        let before = self.narrow_state();
        let covered: Vec<usize> = sels
            .iter()
            .filter_map(|(sel, _)| sel.as_ref())
            .flat_map(|sel| sel.covered.iter().copied())
            .collect();
        let mut exits = vec![];
        let mut breaks = false;
        let mut out = vec![];
        let last = cases.len().saturating_sub(1);
        let mut incoming: Option<Vec<usize>> = None;
        let name = s.local.map(|l| self.f.locals[l.0 as usize].name.clone());
        for (i, (c, (sel, _))) in cases.iter().zip(sels).enumerate() {
            self.restore_narrowing(&before);
            let mut reach: Vec<usize> = match &sel {
                Some(sel) => sel.touched.clone(),
                None => (0..s.slots.len())
                    .filter(|k| !covered.contains(k))
                    .collect(),
            };
            reach.extend(incoming.take().unwrap_or_default());
            self.push_scope_until(c.span.hi);
            for f in self.slot_facts(s, &reach) {
                self.narrow(&f);
            }
            let mut stmts = vec![];
            self.stmts_into(&c.body, &mut stmts);
            self.pop_scope();
            let body = hir::Block {
                stmts,
                value: None,
                span: c.span,
            };
            let falls = !crate::flow::block_diverges(&body, &self.cx.ty);
            // A body that reassigns the value falls into the next one with any member.
            let reassigns = name.as_deref().is_some_and(|n| assigns(&c.body, n));
            incoming = match (falls, reassigns) {
                (false, _) => None,
                (true, false) => Some(reach),
                (true, true) => Some((0..s.slots.len()).collect()),
            };
            // A `break` may leave before facts the rest of the body adds: only those that
            // also held before the `switch` are kept (`before` is met below).
            let breaks_here = breaks_out(&body);
            breaks |= breaks_here;
            if breaks_here || (falls && i == last) {
                exits.push(self.narrow_state());
            }
            out.push(Case { sel, body, falls });
        }
        if let Some(skipped) = skipped {
            self.restore_narrowing(&before);
            for f in self.slot_facts(s, &skipped) {
                self.narrow(&f);
            }
            exits.push(self.narrow_state());
        }
        if breaks {
            exits.push(before.clone());
        }
        // With no way out, what follows is unreachable: the state before is as good as any.
        let mut exits = exits.into_iter();
        self.restore_narrowing(&exits.next().unwrap_or(before));
        for st in exits {
            self.meet_narrowing(&st);
        }
        out
    }

    /// Narrowing of the scrutinee local in a body reached by the values of `reach` (slots).
    fn slot_facts(&mut self, s: &Scrut, reach: &[usize]) -> Vec<Fact> {
        let Some(l) = s.local else {
            return vec![];
        };
        if matches!(s.kind, ScrutKind::Enum(_)) || s.slots.is_empty() {
            return vec![];
        }
        let mut facts = vec![];
        let local_ty = self.local_ty(l);
        let inner = self.cx.ty.opt_payload(local_ty).unwrap_or(local_ty);
        if inner != local_ty && reach.iter().all(|&k| s.slots[k].member.is_some()) {
            facts.push(Fact::NonNull(l));
        }
        if self.cx.union_def(inner).is_some() {
            let vs = reach.iter().filter_map(|&k| s.slots[k].variant).collect();
            facts.push(Fact::Members(l, vs));
        }
        facts
    }

    /// The dispatch statements (see the module docs).
    fn dispatch(
        &mut self,
        s: Scrut,
        cases: Vec<Case>,
        exhaustive: bool,
        span: Span,
    ) -> Vec<hir::Stmt> {
        let n = cases.len();
        let simple = cases
            .iter()
            .enumerate()
            .all(|(i, c)| i + 1 == n || c.body.stmts.is_empty() || !c.falls);
        let groups = if simple { groups(&cases) } else { None };
        match groups {
            Some(g) => vec![self.match_dispatch(s, cases, g, exhaustive, span)],
            None => self.index_dispatch(s, cases, exhaustive, span),
        }
    }

    /// One arm per group of labels sharing a body; `default` last.
    fn match_dispatch(
        &mut self,
        s: Scrut,
        cases: Vec<Case>,
        groups: Vec<(usize, usize)>,
        exhaustive: bool,
        span: Span,
    ) -> hir::Stmt {
        let sty = s.expr.ty;
        let never = self.cx.ty.never;
        let mut cases: Vec<Option<Case>> = cases.into_iter().map(Some).collect();
        let mut arms = vec![];
        let mut default_arm = None;
        for (lo, hi) in groups {
            let group: Vec<Case> = (lo..=hi).filter_map(|i| cases[i].take()).collect();
            let is_default = group.iter().any(|c| c.sel.is_none());
            let body = group
                .last()
                .map(|c| c.body.clone())
                .expect("ICE: empty group");
            let body = self.block_value(body);
            let mut pats = vec![];
            let mut guard = None;
            for c in group {
                if let Some(sel) = c.sel {
                    pats.push(sel.pat);
                    guard = sel.guard.or(guard);
                }
            }
            let pat = match (is_default, pats.len()) {
                (true, _) | (false, 0) => self.pat(P::Wildcard, sty, span),
                (false, 1) => pats.pop().expect("ICE: one pattern"),
                (false, _) => self.pat(P::Or(pats), sty, span),
            };
            let arm = hir::Arm { pat, guard, body };
            if is_default {
                default_arm = Some(arm);
            } else {
                arms.push(arm);
            }
        }
        match default_arm {
            Some(a) => arms.push(a),
            None if exhaustive && s.partial => arms.push(self.unreachable_case(sty, span)),
            None if exhaustive => {}
            None => arms.push(hir::Arm {
                pat: self.pat(P::Wildcard, sty, span),
                guard: None,
                body: self.unit_expr(span),
            }),
        }
        let ty = if exhaustive && arms.iter().all(|a| a.body.ty == never) {
            never
        } else {
            self.cx.ty.unit
        };
        let m = H::Match {
            scrutinee: Box::new(s.expr),
            arms,
        };
        hir::Stmt {
            kind: S::Expr(self.mk(m, ty, span)),
            span,
        }
    }

    /// `let <case> = match (x) { case_i => i, _ => default or n }; if (<case> <= i) { body_i }`.
    fn index_dispatch(
        &mut self,
        s: Scrut,
        cases: Vec<Case>,
        exhaustive: bool,
        span: Span,
    ) -> Vec<hir::Stmt> {
        let (i64_, sty) = (self.cx.ty.i64, s.expr.ty);
        let n = cases.len();
        let int = |cx: &Self, i: usize| cx.mk(H::Lit(hir::Lit::Int(i as u128)), i64_, span);
        let default = cases.iter().position(|c| c.sel.is_none()).unwrap_or(n);
        let mut arms = vec![];
        let mut bodies = vec![];
        for (i, c) in cases.into_iter().enumerate() {
            if let Some(sel) = c.sel {
                arms.push(hir::Arm {
                    pat: sel.pat,
                    guard: sel.guard,
                    body: int(self, i),
                });
            }
            bodies.push(c.body);
        }
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, sty, span),
            guard: None,
            body: int(self, default),
        });
        let idx = self.new_local("<case>", i64_, false, span, LocalKind::Temp);
        let m = self.mk(
            H::Match {
                scrutinee: Box::new(s.expr),
                arms,
            },
            i64_,
            span,
        );
        let mut out = vec![hir::Stmt {
            kind: S::Let {
                local: idx,
                init: Some(m),
            },
            span,
        }];
        let last = bodies.len().saturating_sub(1);
        for (i, body) in bodies.into_iter().enumerate() {
            let kind = if i == last && exhaustive {
                S::Block(body)
            } else {
                let at = self.mk(H::Local(idx, hir::UseMode::Copy), i64_, span);
                let cond = self.mk(
                    H::Binary {
                        op: hir::BinOp::LtEq,
                        lhs: Box::new(at),
                        rhs: Box::new(int(self, i)),
                    },
                    self.cx.ty.bool_,
                    span,
                );
                S::If {
                    cond,
                    then: body,
                    els: None,
                }
            };
            out.push(hir::Stmt { kind, span });
        }
        out
    }

    /// `label: while (true) { stmts; break label; }` — the target of `break`s in the bodies.
    fn run_once(&mut self, mut stmts: Vec<hir::Stmt>, label: String, span: Span) -> hir::Stmt {
        stmts.push(hir::Stmt {
            kind: S::Break(Some(label.clone())),
            span,
        });
        let body = hir::Block {
            stmts,
            value: None,
            span,
        };
        let cond = self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, span);
        hir::Stmt {
            kind: S::While {
                label: Some(label),
                cond,
                body,
                step: None,
            },
            span,
        }
    }

    /// A case body as an arm value: `void`, or `never` when it always leaves the `switch`
    /// (`return` / `throw`; `break` leaves only the arm, so the match still completes).
    fn block_value(&mut self, b: hir::Block) -> hir::Expr {
        let ty = if crate::flow::block_diverges(&b, &self.cx.ty) && !breaks_out(&b) {
            self.cx.ty.never
        } else {
            self.cx.ty.unit
        };
        let span = b.span;
        self.mk(H::Block(b), ty, span)
    }

    /// `_ => panic(...)` for members flow narrowing ruled out.
    fn unreachable_case(&mut self, ty: hir::TyId, span: Span) -> hir::Arm {
        let msg = self.str_lit("unreachable switch case", span);
        let never = self.cx.ty.never;
        hir::Arm {
            pat: self.pat(P::Wildcard, ty, span),
            guard: None,
            body: self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span),
        }
    }
}

/// Does one of `stmts` assign the local named `name`?
fn assigns(stmts: &[ast::Stmt], name: &str) -> bool {
    let mut names = super::assigned::Assigned::new();
    for st in stmts {
        super::assigned::assigned_in_stmt(st, &mut names);
    }
    names.contains_key(name)
}

/// Groups of cases sharing a body (`(first, last)` indices): empty bodies fall into the next
/// case's. `None` when a group would need an or-pattern with guards.
fn groups(cases: &[Case]) -> Option<Vec<(usize, usize)>> {
    let mut out = vec![];
    let mut lo = 0;
    for (i, c) in cases.iter().enumerate() {
        if c.body.stmts.is_empty() && i + 1 < cases.len() {
            continue;
        }
        let guards = cases[lo..=i]
            .iter()
            .filter(|c| c.sel.as_ref().is_some_and(|s| s.guard.is_some()))
            .count();
        if guards > 0 && i > lo {
            return None;
        }
        out.push((lo, i));
        lo = i + 1;
    }
    Some(out)
}

/// Does `b` contain a `break` outside nested loops (one that may leave the `switch`)? Such a
/// body completes the match arm.
fn breaks_out(b: &hir::Block) -> bool {
    b.stmts.iter().any(|s| match &s.kind {
        // A labelled `break` may leave the `switch` too (conservatively: it completes the arm).
        S::Break(_) => true,
        S::Block(inner) => breaks_out(inner),
        S::If { then, els, .. } => breaks_out(then) || els.as_ref().is_some_and(breaks_out),
        S::Try {
            body,
            catch,
            finally,
        } => {
            breaks_out(body)
                || catch.as_ref().is_some_and(|(_, c)| breaks_out(c))
                || finally.as_ref().is_some_and(breaks_out)
        }
        _ => false,
    })
}
