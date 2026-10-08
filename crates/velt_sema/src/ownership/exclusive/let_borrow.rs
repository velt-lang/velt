//! `const x = node.left` bound by reference (`crate::body::const_borrow`): the binding points
//! into `node`, so the rest of its block must not modify or move `node.left` or anything that
//! owns it (`node.left = …`, `node = …`, `node.reset()`), which could free what `x` points to.
//! Changing `x`'s own contents (`x.v = 1`) or unrelated fields (`node.right = …`) is fine.
//! `const me = this` names the object itself, which nothing can replace: only using `me`
//! together with `this` (`super::this_alias`) ends the borrow.
//! When the block does change it, `x` becomes a share of the value instead (semantics stage 2:
//! `x` keeps referring to the object, like in JS); only values that cannot be shared (promises)
//! report the conflict.

use velt_common::{Diagnostic, Span};

use crate::hir::{Block, Expr, ExprKind as E, LocalDef, PatKind, Stmt, StmtKind as S, UseMode};
use crate::visit::{self, VisitMut};

use super::iteration::contains;
use super::this_alias;
use super::uses::{Access, Collector};

/// Check every by-reference `const` in function body `body` (nested blocks included).
pub(super) fn check_body(
    col: &Collector,
    locals: &[LocalDef],
    shared: &[bool],
    body: &mut Block,
    out: &mut Vec<Diagnostic>,
) {
    let mut v = Lets {
        col,
        locals,
        shared,
        out,
    };
    v.scan(body);
    visit::block(body, &mut v);
}

/// Visits every block of a body once: the statements of each are checked in `scan`.
struct Lets<'c, 'a, 'm> {
    col: &'c Collector<'a, 'm>,
    locals: &'c [LocalDef],
    /// Per local: is its value shared (`Ctx::is_shared_value`)?
    shared: &'c [bool],
    out: &'c mut Vec<Diagnostic>,
}

impl Lets<'_, '_, '_> {
    fn scan(&mut self, b: &mut Block) {
        for i in 0..b.stmts.len() {
            let (head, rest) = b.stmts.split_at_mut(i + 1);
            let value = b.value.as_deref_mut();
            if let Some(d) = check_let(self.col, self.locals, &mut head[i], rest, value) {
                match share_binding(&mut head[i], self.shared) {
                    true => {}
                    false => self.out.push(d),
                }
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
    if let Some(this) = this_local(col, init) {
        return this_alias::conflict(col, l, this, rest, value).map(|span| {
            let name = &col.locals[l.0 as usize].name;
            changed_while_borrowed("this", Access::Unique, span, name, "this", at)
        });
    }
    col.nested_stmts(rest, value, &mut uses);
    let hit = uses
        .into_iter()
        .find(|u| u.access != Access::Shared && contains(&u.place, &place))?;
    let name = &locals[l.0 as usize].name;
    Some(changed_while_borrowed(
        &hit.text, hit.access, hit.span, name, &text, at,
    ))
}

/// `this` when `init` is the method's own `this` (`const me = this`, `crate::body::const_borrow`;
/// no other local can be named `this`).
fn this_local(col: &Collector, init: &Expr) -> Option<crate::hir::LocalId> {
    match init.kind {
        E::Local(l, _) if col.locals[l.0 as usize].name == "this" => Some(l),
        _ => None,
    }
}

/// Turn the by-reference `const` `s` into an owned share of the place it referred to; false
/// when its value cannot be shared.
fn share_binding(s: &mut Stmt, shared: &[bool]) -> bool {
    let S::LetPat { pat, init } = &mut s.kind else {
        return false;
    };
    let PatKind::Binding(l, UseMode::Borrow) = pat.kind else {
        return false;
    };
    if !shared[l.0 as usize] {
        return false;
    }
    let mut init = init.clone();
    super::super::soft::make_share(&mut init);
    s.kind = S::Let {
        local: l,
        init: Some(init),
    };
    true
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
