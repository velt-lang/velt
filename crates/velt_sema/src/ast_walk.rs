//! Direct sub-expressions of a surface expression, for passes that scan the AST before
//! checking (nested declarations) or rewrite it (local generic arrows). Statements inside
//! block-bodied arrow functions and function expressions are not expressions and are left to
//! the caller.

use velt_syntax::ast::{self, ExprKind as E, JsxAttr, JsxAttrValue, JsxChild, ObjectProp};

/// Call `f` on every direct sub-expression of `e`.
pub(crate) fn children<'a>(e: &'a ast::Expr, f: &mut dyn FnMut(&'a ast::Expr)) {
    match &e.kind {
        E::Lit(_) | E::NumArray(_) | E::Ident(_) | E::This | E::Super | E::Function(_) => {}
        E::Template { exprs, .. } | E::Array(exprs) => exprs.iter().for_each(f),
        E::Unary { expr, .. }
        | E::Update { target: expr, .. }
        | E::Spread(expr)
        | E::Await(expr)
        | E::Cast { expr, .. }
        | E::InstanceOf { expr, .. }
        | E::Paren(expr)
        | E::NonNull(expr)
        | E::Unsafe(expr)
        | E::Member { object: expr, .. } => f(expr),
        E::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { target, value, .. } => {
            f(target);
            f(value);
        }
        E::Cond { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        E::New { args, .. } => args.iter().for_each(f),
        E::Index {
            object,
            index,
            more,
            ..
        } => {
            f(object);
            f(index);
            more.iter().for_each(f);
        }
        E::Arrow { body, .. } => {
            if let ast::ArrowBody::Expr(x) = body {
                f(x);
            }
        }
        E::Object(props) | E::StructLit { props, .. } => {
            for p in props {
                match p {
                    ObjectProp::KeyValue(_, v) | ObjectProp::Spread(v) => f(v),
                    ObjectProp::Shorthand(_) | ObjectProp::Method(_) => {}
                }
            }
        }
        E::Jsx(element) => jsx_exprs(element, f),
        E::Yield { arg: Some(a), .. } => f(a),
        E::Yield { arg: None, .. } => {}
    }
}

/// Call `f` on every expression of a JSX element: attribute values, spreads and children,
/// descending into nested elements (which are not expressions themselves).
fn jsx_exprs<'a>(el: &'a ast::JsxElement, f: &mut dyn FnMut(&'a ast::Expr)) {
    for attr in &el.attrs {
        match attr {
            JsxAttr::Spread { expr, .. } => f(expr),
            JsxAttr::Named { value, .. } => match value {
                Some(JsxAttrValue::Expr { expr, .. }) => f(expr),
                Some(JsxAttrValue::Element(inner)) => jsx_exprs(inner, f),
                Some(JsxAttrValue::Str { .. }) | None => {}
            },
        }
    }
    for child in &el.children {
        match child {
            JsxChild::Expr {
                expr: Some(expr), ..
            }
            | JsxChild::Spread { expr, .. } => f(expr),
            JsxChild::Element(inner) => jsx_exprs(inner, f),
            JsxChild::Text { .. } | JsxChild::Expr { expr: None, .. } => {}
        }
    }
}

/// Call `f` on the name each component tag of JSX element `el` reads (`Card`, `ui` of `ui.Card`):
/// `el` and the elements nested in it. Intrinsic tags read nothing.
pub(crate) fn jsx_tag_roots<'a>(el: &'a ast::JsxElement, f: &mut dyn FnMut(&'a ast::Ident)) {
    if let Some(id) = el.tag_root() {
        f(id);
    }
    for attr in &el.attrs {
        if let JsxAttr::Named {
            value: Some(JsxAttrValue::Element(inner)),
            ..
        } = attr
        {
            jsx_tag_roots(inner, f);
        }
    }
    for child in &el.children {
        if let JsxChild::Element(inner) = child {
            jsx_tag_roots(inner, f);
        }
    }
}

/// Does `e` name `name` inside an arrow or function expression?
pub(crate) fn reads_in_fn_value(e: &velt_syntax::ast::Expr, name: &str) -> bool {
    use velt_syntax::ast::ExprKind as E;
    use velt_syntax::visit::{walk_expr, Visit};
    struct Names<'n>(&'n str, bool);
    impl Visit<'_> for Names<'_> {
        fn expr(&mut self, e: &velt_syntax::ast::Expr) {
            self.1 |= matches!(&e.kind, E::Ident(i) if i.name == self.0);
            if let E::Jsx(el) = &e.kind {
                jsx_tag_roots(el, &mut |id| self.1 |= id.name == self.0);
            }
        }
    }
    struct Fns<'n>(&'n str, bool);
    impl Visit<'_> for Fns<'_> {
        fn expr(&mut self, e: &velt_syntax::ast::Expr) {
            if !self.1 && matches!(e.kind, E::Arrow { .. } | E::Function(_)) {
                let mut n = Names(self.0, false);
                walk_expr(e, &mut n);
                self.1 = n.1;
            }
        }
    }
    let mut f = Fns(name, false);
    walk_expr(e, &mut f);
    f.1
}

/// Like [`children`], mutably.
pub(crate) fn children_mut(e: &mut ast::Expr, f: &mut dyn FnMut(&mut ast::Expr)) {
    match &mut e.kind {
        E::Lit(_) | E::NumArray(_) | E::Ident(_) | E::This | E::Super | E::Function(_) => {}
        E::Template { exprs, .. } | E::Array(exprs) | E::New { args: exprs, .. } => {
            exprs.iter_mut().for_each(f)
        }
        E::Unary { expr, .. }
        | E::Update { target: expr, .. }
        | E::Spread(expr)
        | E::Await(expr)
        | E::Cast { expr, .. }
        | E::InstanceOf { expr, .. }
        | E::NonNull(expr)
        | E::Unsafe(expr)
        | E::Paren(expr)
        | E::Member { object: expr, .. } => f(expr),
        E::Binary { lhs: a, rhs: b, .. }
        | E::Assign {
            target: a,
            value: b,
            ..
        } => {
            f(a);
            f(b);
        }
        E::Index {
            object,
            index,
            more,
            ..
        } => {
            f(object);
            f(index);
            more.iter_mut().for_each(f);
        }
        E::Cond { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::Call { callee, args, .. } => {
            f(callee);
            args.iter_mut().for_each(f);
        }
        E::Arrow { body, .. } => {
            if let ast::ArrowBody::Expr(x) = body {
                f(x);
            }
        }
        E::Object(props) | E::StructLit { props, .. } => {
            for p in props {
                match p {
                    ObjectProp::KeyValue(_, v) | ObjectProp::Spread(v) => f(v),
                    ObjectProp::Shorthand(_) | ObjectProp::Method(_) => {}
                }
            }
        }
        E::Yield { arg: Some(a), .. } => f(a),
        E::Yield { arg: None, .. } => {}
        E::Jsx(element) => jsx_exprs_mut(element, f),
    }
}

fn jsx_exprs_mut(el: &mut ast::JsxElement, f: &mut dyn FnMut(&mut ast::Expr)) {
    for attr in &mut el.attrs {
        match attr {
            JsxAttr::Spread { expr, .. } => f(expr),
            JsxAttr::Named { value, .. } => match value {
                Some(JsxAttrValue::Expr { expr, .. }) => f(expr),
                Some(JsxAttrValue::Element(inner)) => jsx_exprs_mut(inner, f),
                Some(JsxAttrValue::Str { .. }) | None => {}
            },
        }
    }
    for child in &mut el.children {
        match child {
            JsxChild::Expr {
                expr: Some(expr), ..
            }
            | JsxChild::Spread { expr, .. } => f(expr),
            JsxChild::Element(inner) => jsx_exprs_mut(inner, f),
            JsxChild::Text { .. } | JsxChild::Expr { expr: None, .. } => {}
        }
    }
}
