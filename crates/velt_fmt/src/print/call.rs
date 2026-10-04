//! Calls and argument lists (prettier's `printCallArguments`): arguments stay on one line when
//! they fit; a trailing callback, object or array literal is "hugged" (`f(a, (x) => {` ... `})`)
//! rather than moving every argument to its own line; otherwise one argument per line.

use velt_syntax::ast::{ArrowBody, Expr, ExprKind, Ident, TypeExpr};

use super::lists::delimited;
use super::Printer;
use crate::doc::{
    break_parent, cat, conditional, expanded, group_broken, if_break, indent, join, line, nil,
    softline, text, Doc,
};

impl<'a> Printer<'a> {
    /// The call `e` (with the given parts); calls on member accesses are laid out as member
    /// chains.
    pub(super) fn call(
        &mut self,
        e: &Expr,
        callee: &Expr,
        type_args: &[TypeExpr],
        args: &[Expr],
        optional: bool,
    ) -> Doc {
        if is_memberish(callee) {
            return self.member_chain(e);
        }
        let callee = self.expr(callee);
        cat![
            callee,
            self.call_suffix(type_args, args, optional, e.span.hi)
        ]
    }

    /// `?.<T>(args)` after a callee; `end` is the end of the call.
    pub(super) fn call_suffix(
        &mut self,
        type_args: &[TypeExpr],
        args: &[Expr],
        optional: bool,
        end: u32,
    ) -> Doc {
        let optional = if optional { "?." } else { "" };
        cat![optional, self.type_args(type_args), self.args(args, end)]
    }

    /// `<A, B>`, or nothing.
    pub(super) fn type_args(&mut self, tys: &[TypeExpr]) -> Doc {
        if tys.is_empty() {
            return nil();
        }
        let docs = tys.iter().map(|t| self.ty(t)).collect();
        cat!["<", join(&text(", "), docs), ">"]
    }

    /// `(args)` of a call or `new`; `end` is the end of the whole expression.
    pub(super) fn args(&mut self, args: &[Expr], end: u32) -> Doc {
        let list = self.list(args, end, |a| (a.span.lo, a.span.hi), |p, a| p.expr(a));
        if list.has_comments || !hug_last(args) {
            return delimited("(", list, ")", false);
        }
        let docs = list.items;
        let (last, init) = match docs.split_last() {
            Some((last, init)) => (last.clone(), init.to_vec()),
            None => return text("()"),
        };
        let broken_out = group_broken(cat![
            "(",
            indent(cat![softline(), join(&cat![",", line()], docs.clone())]),
            if_break(",", ""),
            softline(),
            ")"
        ]);
        if init.iter().any(Doc::breaks) {
            return broken_out;
        }
        let any_breaks = docs.iter().any(Doc::breaks);
        let flat = cat!["(", join(&text(", "), docs), ")"];
        let head = if init.is_empty() {
            nil()
        } else {
            cat![join(&text(", "), init), ", "]
        };
        let hugged = cat!["(", head, expanded(&last), ")"];
        let force = if any_breaks { break_parent() } else { nil() };
        cat![force, conditional(vec![flat, hugged, broken_out])]
    }

    /// `[index]` / `?.[index]`.
    pub(super) fn index_suffix(&mut self, index: &Expr, optional: bool) -> Doc {
        let open = if optional { "?.[" } else { "[" };
        cat![open, self.expr(index), "]"]
    }
}

/// `.prop` / `?.prop`.
pub(super) fn member_suffix(prop: &Ident, optional: bool) -> Doc {
    // A symbol key (`x[Symbol.dispose]`) is a bracketed suffix of its own.
    let dot = match (optional, prop.name.starts_with('[')) {
        (true, _) => "?.",
        (false, true) => "",
        (false, false) => ".",
    };
    cat![dot, prop.name.clone()]
}

/// Member access or indexing (a call on one is a member chain).
pub(super) fn is_memberish(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Member { .. } | ExprKind::Index { .. })
}

/// Should the last argument be hugged? It must be a literal/callback that breaks well, and not
/// the same kind as the argument before it (two callbacks read better one per line).
fn hug_last(args: &[Expr]) -> bool {
    let Some((last, init)) = args.split_last() else {
        return false;
    };
    if !huggable(last) {
        return false;
    }
    match init.last() {
        Some(prev) => std::mem::discriminant(&prev.kind) != std::mem::discriminant(&last.kind),
        None => true,
    }
}

fn huggable(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Object(props) | ExprKind::StructLit { props, .. } => !props.is_empty(),
        ExprKind::Array(elems) => !elems.is_empty(),
        ExprKind::Arrow { body, .. } => match body {
            ArrowBody::Block(_) => true,
            ArrowBody::Expr(e) => super::jsx::is_jsx_layout(e),
        },
        ExprKind::Function(_) => true,
        _ => false,
    }
}
