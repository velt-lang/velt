//! Expressions for the scope walker: descend into the one child that contains the cursor, opening
//! scopes for arrow functions, function expressions and match arms.

use velt_syntax::ast;

use super::{Reference, Walker};
use crate::signature;

impl<'a> Walker<'a> {
    pub(super) fn opt_expr(&mut self, e: Option<&'a ast::Expr>) {
        if let Some(e) = e {
            self.expr(e);
        }
    }

    fn exprs(&mut self, es: &'a [ast::Expr]) {
        es.iter().for_each(|e| self.expr(e));
    }

    /// Walk `e` if it contains the cursor.
    pub(super) fn expr(&mut self, e: &'a ast::Expr) {
        if !self.contains(e.span) {
            return;
        }
        match &e.kind {
            ast::ExprKind::Ident(ident) => self.value_name(ident),
            ast::ExprKind::Member { object, prop, .. } => self.member(object, prop),
            ast::ExprKind::Arrow {
                params,
                ret,
                throws,
                body,
                ..
            } => self.arrow(params, [ret.as_ref(), throws.as_ref()], body),
            ast::ExprKind::Function(f) => self.function(&f.sig, Some(&f.body)),
            ast::ExprKind::Object(props) => self.props(props),
            ast::ExprKind::StructLit { name, props } => {
                self.ty(name);
                self.props(props);
            }
            ast::ExprKind::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                self.expr(callee);
                type_args.iter().for_each(|t| self.ty(t));
                self.exprs(args);
            }
            ast::ExprKind::New { class, args } => {
                self.ty(class);
                self.exprs(args);
            }
            ast::ExprKind::Cast { expr, ty } | ast::ExprKind::InstanceOf { expr, ty } => {
                self.expr(expr);
                self.ty(ty);
            }
            ast::ExprKind::Jsx(el) => self.jsx(el),
            _ => self.plain_children(e),
        }
    }

    /// Expressions whose children need no scope or type handling.
    fn plain_children(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            ast::ExprKind::Template { exprs, .. } | ast::ExprKind::Array(exprs) => {
                self.exprs(exprs)
            }
            ast::ExprKind::Unary { expr, .. }
            | ast::ExprKind::Spread(expr)
            | ast::ExprKind::Await(expr)
            | ast::ExprKind::Paren(expr) => self.expr(expr),
            ast::ExprKind::Yield { arg: Some(a), .. } => self.expr(a),
            ast::ExprKind::Yield { arg: None, .. } => {}
            ast::ExprKind::Update { target, .. } => self.expr(target),
            ast::ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ast::ExprKind::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            ast::ExprKind::Cond { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            ast::ExprKind::Index { object, index, .. } => {
                self.expr(object);
                self.expr(index);
            }
            _ => {}
        }
    }

    fn member(&mut self, object: &'a ast::Expr, prop: &'a ast::Ident) {
        if !self.contains(prop.span) {
            self.expr(object);
        } else if matches!(object.kind, ast::ExprKind::This) {
            self.hit(Reference::ThisMember(prop));
        } else {
            self.hit(Reference::Member { object, prop });
        }
    }

    fn arrow(
        &mut self,
        params: &'a [ast::ArrowParam],
        ret_throws: [Option<&'a ast::TypeExpr>; 2],
        body: &'a ast::ArrowBody,
    ) {
        self.scoped(|w| {
            for p in params {
                w.opt_ty(p.ty.as_ref());
                let detail = signature::arrow_param(w.analysis, p);
                let ty =
                    p.ty.as_ref()
                        .and_then(crate::index::type_name)
                        .map(String::from);
                w.bind(&p.name, detail, ty);
            }
            ret_throws.into_iter().for_each(|t| w.opt_ty(t));
            match body {
                ast::ArrowBody::Expr(e) => w.expr(e),
                ast::ArrowBody::Block(b) => w.block(b),
            }
        });
    }

    fn props(&mut self, props: &'a [ast::ObjectProp]) {
        for prop in props {
            match prop {
                ast::ObjectProp::KeyValue(_, value) => self.expr(value),
                ast::ObjectProp::Shorthand(ident) => self.value_name(ident),
                ast::ObjectProp::Spread(e) => self.expr(e),
                ast::ObjectProp::Method(f) if self.contains(f.body.span) => {
                    self.function(&f.sig, Some(&f.body))
                }
                ast::ObjectProp::Method(_) => {}
            }
        }
    }
}
