//! An object literal's method for an interface method declared `: this` (#3771). A call of
//! such a method through a type parameter, or on a value of an interface that extends the
//! declaring one, gives the receiver itself (`iface_call`, `crate::hand_back`), which holds
//! because every implementation returns `this`: a class's is checked by `check_this_return`,
//! and a literal's here. TypeScript types `this` in a literal as the interface and so accepts
//! any value of it (`step(n) { return other; }`); that method is refused, since the call would
//! give the literal where Node gives `other`.
//!
//! What a literal's method may return: `this`, or the result of another `: this` method of the
//! interface called on that (`return this.step(1);`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::DefId;

/// The values the method written at `root` returns, and the functions nested in it (whose
/// `return`s are their own).
struct Returned<'a> {
    root: Span,
    values: Vec<&'a ast::Expr>,
    nested: Vec<Span>,
}

impl<'a> velt_syntax::visit::Visit<'a> for Returned<'a> {
    fn stmt(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            ast::StmtKind::Return(Some(e)) => self.values.push(e),
            ast::StmtKind::Item(_) => self.nested.push(s.span),
            _ => {}
        }
    }
    fn expr(&mut self, e: &'a ast::Expr) {
        // A `return` inside an object literal is one of a method of that literal.
        let function = matches!(
            e.kind,
            ast::ExprKind::Arrow { .. }
                | ast::ExprKind::Function(_)
                | ast::ExprKind::Object(_)
                | ast::ExprKind::StructLit { .. }
        );
        if function && e.span != self.root {
            self.nested.push(e.span);
        }
    }
}

/// The values method `e` (an arrow) returns: its expression body, or the value of each
/// `return` of its own. `None` when `e` is no arrow.
fn returned(e: &ast::Expr) -> Option<Vec<&ast::Expr>> {
    let ast::ExprKind::Arrow { body, .. } = &e.kind else {
        return None;
    };
    let block = match body {
        ast::ArrowBody::Expr(value) => return Some(vec![value]),
        ast::ArrowBody::Block(block) => block,
    };
    let mut r = Returned {
        root: e.span,
        values: vec![],
        nested: vec![],
    };
    velt_syntax::visit::walk_block(block, &mut r);
    let nested = r.nested;
    let own = |at: &Span| !nested.iter().any(|n| n.lo <= at.lo && at.hi <= n.hi);
    Some(r.values.into_iter().filter(|v| own(&v.span)).collect())
}

impl FnCx<'_, '_> {
    /// Is `e` the method's `this`, or the result of a `: this` method of `iface` called on it?
    fn literal_this_value(&self, e: &ast::Expr, iface: DefId) -> bool {
        match &e.kind {
            ast::ExprKind::This => true,
            ast::ExprKind::Paren(x) => self.literal_this_value(x, iface),
            ast::ExprKind::Call {
                callee,
                optional: false,
                ..
            } => match &callee.kind {
                ast::ExprKind::Member {
                    object,
                    prop,
                    optional: false,
                } => {
                    crate::hand_back::iface_returns_this(self.cx, iface, &prop.name)
                        && self.literal_this_value(object, iface)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Member `key` (written as `value`) of an object literal implementing `iface` (named
    /// `iname`) for a method declared `: this`: reported unless it is a method returning
    /// `this` (module docs). Whether it is.
    pub(super) fn literal_returns_this(
        &mut self,
        key: &ast::Ident,
        value: Option<&ast::Expr>,
        (iface, iname): (DefId, &str),
    ) -> bool {
        if !crate::hand_back::iface_returns_this(self.cx, iface, &key.name) {
            return true;
        }
        let method = value.filter(|v| super::literal_this::takes_this(v));
        let at = match method.and_then(returned) {
            Some(values) => match values.iter().find(|v| !self.literal_this_value(v, iface)) {
                Some(other) => other.span,
                None => return true,
            },
            // A function that is no method of the literal (an arrow, a variable): its `this`
            // is not the object.
            None => value.map_or(key.span, |v| v.span),
        };
        let name = &key.name;
        self.cx.error(
            Diagnostic::error(
                format!("method `{name}` of an object literal implementing `{iname}` must return `this`"),
                at,
            )
            .with_note(format!("`{name}` is declared `: this`: a call gives the object it was called on, so every implementation returns it. TypeScript accepts any `{iname}` here"))
            .with_note(format!("write it as a method that returns `this` (`{name}(…) {{ …; return this; }}`); for a method that may return another object, declare the interface's result as the interface instead of `this`")),
        );
        false
    }
}
