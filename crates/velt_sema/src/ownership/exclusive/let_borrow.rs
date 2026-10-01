//! `const x = node.left` bound by reference (`crate::body::const_borrow`): the binding points
//! into `node`, so the rest of its block must not modify or move `node.left` or anything that
//! owns it (`node.left = …`, `node = …`, `node.reset()`), which could free what `x` points to.
//! Changing `x`'s own contents (`x.v = 1`) or unrelated fields (`node.right = …`) is fine.

use velt_common::{Diagnostic, Span};

use crate::hir::{Block, Expr, ExprKind as E, LocalDef, PatKind, Stmt, StmtKind as S, UseMode};
use crate::visit::{self, VisitMut};

use super::iteration::contains;
use super::uses::{Access, Collector};

/// Check every by-reference `const` in function body `body` (nested blocks included).
pub(super) fn check_body(
    col: &Collector,
    locals: &[LocalDef],
    body: &mut Block,
    out: &mut Vec<Diagnostic>,
) {
    let mut v = Lets { col, locals, out };
    v.scan(body);
    visit::block(body, &mut v);
}

/// Visits every block of a body once: the statements of each are checked in `scan`.
struct Lets<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    locals: &'c [LocalDef],
    out: &'c mut Vec<Diagnostic>,
}

impl Lets<'_, '_, '_> {
    fn scan(&mut self, b: &mut Block) {
        for i in 0..b.stmts.len() {
            let (head, rest) = b.stmts.split_at_mut(i + 1);
            let value = b.value.as_deref_mut();
            if let Some(d) = check_let(self.col, self.locals, &mut head[i], rest, value) {
                self.out.push(d);
            }
        }
    }
}

impl VisitMut for Lets<'_, '_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            S::If { then, els, .. } => {
                self.scan(then);
                if let Some(b) = els {
                    self.scan(b);
                }
            }
            S::While { body, .. } | S::ForOf { body, .. } | S::Block(body) => self.scan(body),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.scan(body);
                if let Some((_, b)) = catch {
                    self.scan(b);
                }
                if let Some(b) = finally {
                    self.scan(b);
                }
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        if let E::Block(b) = &mut e.kind {
            self.scan(b);
        }
    }
}

/// The first change of the place `s` (a by-reference `const`) borrows, in the statements after it.
fn check_let(
    col: &Collector,
    locals: &[LocalDef],
    s: &mut Stmt,
    rest: &mut [Stmt],
    value: Option<&mut Expr>,
) -> Option<Diagnostic> {
    let S::LetPat { pat, init } = &s.kind else {
        return None;
    };
    let PatKind::Binding(l, UseMode::Borrow) = pat.kind else {
        return None;
    };
    let (place, _) = col.place_of(init)?;
    let at = init.span;
    let text = col.text(init);
    let mut uses = vec![];
    col.nested_stmts(rest, value, &mut uses);
    let hit = uses
        .into_iter()
        .find(|u| u.access != Access::Shared && contains(&u.place, &place))?;
    let name = &locals[l.0 as usize].name;
    Some(changed_while_borrowed(
        &hit.text, hit.access, hit.span, name, &text, at,
    ))
}

fn changed_while_borrowed(
    what: &str,
    access: Access,
    span: Span,
    name: &str,
    text: &str,
    at: Span,
) -> Diagnostic {
    let verb = if access == Access::Move {
        "move"
    } else {
        "modify"
    };
    Diagnostic::error(
        format!("cannot {verb} `{what}` while `{name}` refers to `{text}`"),
        span,
    )
    .with_label(at, format!("`{name}` refers to `{text}` from here"))
    .with_note(format!(
        "`{name}` is the value stored in `{text}`, not a copy; declare `{name}` after the change, or copy the value with `.clone()`"
    ))
}
