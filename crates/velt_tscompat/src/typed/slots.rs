//! What goes into the parameters and fields the program declares: a value that is `undefined`
//! in JavaScript for a required `T | null` (`undefined-into-null`, see [`nulls`](super::nulls)).
//! Arguments of functions, methods and constructors, object literal fields and assignments to
//! fields. (`null` for an optional parameter or field is TS2322 in `velt check` itself, as in
//! `tsc`, since an optional one is `T | undefined` in Velt too, #930.)

use velt_common::Span;
use velt_sema::ide::{DefKind, TypeView};
use velt_syntax::ast::{self, ExprKind as E};

use super::nulls::{check_into_null, ident_source};
use super::Typed;
use crate::{Fix, Severity};

/// `o.f = v`: an `undefined` into a required `T | null` field.
pub(super) fn assign(target: &ast::Expr, value: &ast::Expr, t: &mut Typed) {
    let E::Member { prop, .. } = &target.kind else {
        return;
    };
    let Some(d) = t.def(prop.span).filter(|d| d.kind == DefKind::Field) else {
        return;
    };
    if !t.decls.is_optional(d.span) {
        check_into_null(value, t);
    }
}

/// `{ f: v }`: the same for the fields of an object literal.
pub(super) fn object(props: &[ast::ObjectProp], t: &mut Typed) {
    for prop in props {
        let (key, value) = match prop {
            ast::ObjectProp::KeyValue(key, value) => (key, Some(value)),
            ast::ObjectProp::Shorthand(key) => (key, None),
            ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => continue,
        };
        let Some(d) = t.def(key.span).filter(|d| d.kind == DefKind::Field) else {
            continue;
        };
        let optional = t.decls.is_optional(d.span);
        match value {
            Some(v) if !optional => check_into_null(v, t),
            None if !optional => {
                if let Some(why) = ident_source(key, t) {
                    let fix = Fix {
                        span: key.span,
                        replacement: format!("{0}: {0} ?? null", key.name),
                        title: "turn `undefined` into `null`: `?? null`".into(),
                    };
                    t.cx.report(
                        "undefined-into-null",
                        Severity::Error,
                        key.span,
                        "TypeScript types this value `T | undefined`, which a `T | null` \
                         doesn't accept"
                            .into(),
                        &[why, "write `name: name ?? null`"],
                        Some(fix),
                    );
                }
            }
            _ => {}
        }
    }
}

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
    for (arg, param) in args.iter().zip(&params) {
        if !t.decls.is_optional(*param) && param_nullable(*param, t) {
            check_into_null(arg, t);
        }
    }
}

/// The parameter declared at `param` has a `T | null` type.
fn param_nullable(param: Span, t: &Typed) -> bool {
    t.program
        .analysis
        .type_of(param)
        .is_some_and(|ty| matches!(t.view(&ty), TypeView::Nullable(_)))
}
