//! The arguments of calls to functions, methods and constructors the program declares, for
//! the rules that check them against the declared parameters (`int-division`).

use velt_common::Span;
use velt_sema::ide::DefKind;
use velt_syntax::ast::{self, ExprKind as E};

use super::Typed;

/// The arguments of a call to a function or method declared in the program.
pub(super) fn call(callee: &ast::Expr, args: &[ast::Expr], t: &mut Typed) {
    let name = match &callee.kind {
        E::Ident(id) => id.span,
        E::Member { prop, .. } => prop.span,
        _ => return,
    };
    let Some(d) = t.def(name) else { return };
    if matches!(
        d.kind,
        DefKind::Function | DefKind::Method | DefKind::StaticMethod
    ) {
        arguments(d.span, args, t);
    }
}

/// `new C(…)`: the constructor's arguments.
pub(super) fn new(class: &ast::TypeExpr, args: &[ast::Expr], t: &mut Typed) {
    let ast::TypeExprKind::Named { path, .. } = &class.kind else {
        return;
    };
    let Some(name) = path.last() else { return };
    if let Some(d) = t.def(name.span).filter(|d| d.kind == DefKind::Class) {
        arguments(d.span, args, t);
    }
}

fn arguments(owner: Span, args: &[ast::Expr], t: &mut Typed) {
    if args.iter().any(|a| matches!(a.kind, E::Spread(_))) {
        return;
    }
    let Some(params) = t.decls.params.get(&owner).cloned() else {
        return;
    };
    super::numbers::arguments(&params, args, t);
}
