//! `const me = this` bound by reference (`crate::body::const_borrow`): `me` and `this` name one
//! object, and nothing can replace `this`, so changing the object through either name is fine.
//! What the borrow cannot allow is a move of `this` itself (its last owner may free it), and
//! the two names meeting where the checks below treat them as one place: as two arguments of
//! one call (`link(me, this)`), as an iterated array, a matched value or a destructured value
//! (`for (const x of me.items)`), where `this` would conflict with itself. There `me` becomes a
//! share instead, as before (`super::let_borrow`).

use velt_common::Span;

use crate::hir::{Def, Expr, ExprKind as E, LocalId, Stmt, StmtKind as S};
use crate::visit::{self, VisitMut};

use super::uses::{Access, Collector};

/// Where the statements after `const me = this` (`me`, `this`) stop `me` from referring to
/// `this`, if they do.
pub(super) fn conflict(
    col: &Collector,
    me: LocalId,
    this: LocalId,
    rest: &mut [Stmt],
    value: Option<&mut Expr>,
) -> Option<Span> {
    let mut v = Meet {
        col,
        me,
        this,
        hit: None,
    };
    for s in rest.iter_mut() {
        visit::stmt(s, &mut v);
    }
    if let Some(e) = value {
        visit::expr(e, &mut v);
    }
    v.hit
}

struct Meet<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    me: LocalId,
    this: LocalId,
    hit: Option<Span>,
}

impl Meet<'_, '_, '_> {
    /// The roots each argument uses (directly or while evaluated), per argument.
    fn roots(&self, args: &mut [Expr]) -> Vec<Vec<LocalId>> {
        let mut out = vec![];
        for a in args.iter_mut() {
            let (mut direct, mut nested) = (vec![], vec![]);
            self.col.direct(a, &mut direct, &mut nested);
            out.push(direct.iter().chain(&nested).map(|u| u.place.root).collect());
        }
        out
    }

    /// Does an argument use `me` while another one uses `me` or `this`? `extra`: the variables
    /// a held closure the call runs captures, as one more argument.
    fn meets(&self, args: &mut [Expr], extra: Option<Vec<LocalId>>) -> bool {
        let mut roots = self.roots(args);
        roots.extend(extra);
        let either = |rs: &Vec<LocalId>| rs.iter().any(|r| *r == self.me || *r == self.this);
        roots.iter().enumerate().any(|(i, ri)| {
            ri.contains(&self.me) && roots.iter().enumerate().any(|(j, rj)| i != j && either(rj))
        })
    }

    /// The enclosing variables closure `c` captures.
    fn captured(&self, c: crate::hir::DefId) -> Vec<LocalId> {
        match &self.col.cx.defs[c.0 as usize] {
            Some(Def::Fn(f)) => f.captures.iter().map(|cap| cap.outer).collect(),
            _ => vec![],
        }
    }

    /// Is `e` a place rooted at `me`?
    fn rooted_at_me(&self, e: &Expr) -> bool {
        self.col.place_of(e).is_some_and(|(p, _)| p.root == self.me)
    }

    fn found(&mut self, span: Span) {
        self.hit.get_or_insert(span);
    }
}

impl VisitMut for Meet<'_, '_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::ForOf { iter, .. } if self.rooted_at_me(iter) => self.found(iter.span),
            S::LetPat { init, .. } if self.rooted_at_me(init) => self.found(init.span),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        let span = e.span;
        match &mut e.kind {
            E::Call { callee, args } => {
                let extra = self.col.held_call(callee).map(|c| self.captured(c));
                if self.meets(args, extra) {
                    self.found(span);
                }
            }
            E::New { args, .. } => {
                if self.meets(args, None) {
                    self.found(span);
                }
            }
            E::Match { scrutinee, .. } if self.rooted_at_me(scrutinee) => self.found(span),
            _ => {
                // A move of `this` itself: its owner may free the object `me` names.
                if let Some((p, mode)) = self.col.place_of(e) {
                    let moved = super::uses::access_of(mode) == Some(Access::Move);
                    if moved && p.root == self.this && p.proj.is_empty() {
                        self.found(span);
                    }
                }
            }
        }
    }
}
