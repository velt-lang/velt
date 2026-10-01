//! Setters (`set name(v: T) { ... }`, docs/reference/classes.md): `x.name = v` on a value
//! whose type has a setter `name` (and no field of that name) is the call `x.set name(v)` —
//! setters live under their own method-table key ([`member_key`]), so a getter and a setter
//! may share a name. Compound assignment and `++`/`--` read through the getter and write
//! through the setter: `x.name += v` is `x.set name(x.name + v)`, which evaluates the receiver
//! twice, so it is only allowed on receivers without side effects (variables, `this`, field
//! paths).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::defs::member_key;
use crate::hir::{self, TyId};

/// `(object, prop)` of an assignment target `object.prop` (parentheses removed).
fn member_parts(target: &ast::Expr) -> Option<(&ast::Expr, &ast::Ident)> {
    match &target.kind {
        ast::ExprKind::Paren(inner) => member_parts(inner),
        ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } => Some((object, prop)),
        _ => None,
    }
}

/// Evaluating `e` twice is the same as once: a variable, `this` or a field path of those.
fn side_effect_free(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(x) => side_effect_free(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => side_effect_free(object),
        _ => false,
    }
}

fn synth(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

impl FnCx<'_, '_> {
    /// Does type `t` have a setter `name` (and no field of that name)?
    pub(crate) fn has_setter(&mut self, t: TyId, name: &str) -> bool {
        self.field_of(t, name).is_none()
            && self.resolve_method(t, &member_key(name, true)).is_some()
    }

    /// `obj.prop = arg` through the setter (`obj` is the checked receiver).
    fn setter_call(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        arg: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let key = ast::Ident {
            name: member_key(&prop.name, true),
            span: prop.span,
        };
        let args = std::slice::from_ref(arg);
        // Setters modify `this`; say so in terms of the assignment.
        if !self.require_mutable(&obj, "assign to a property of") {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        self.method_call_on(obj, &key, &[], args, None, span)
    }

    /// `target = value` / `target op= value` where `target` names a setter of `obj`'s type.
    pub(super) fn setter_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let (object, prop) = member_parts(target).expect("ICE: setter target is a member");
        let Some(op) = op else {
            return self.setter_call(obj, prop, value, span);
        };
        if !self.read_write_ok(obj.ty, object, prop, "compound assignment") {
            self.check_args_loose(std::slice::from_ref(value));
            return self.error_expr(span);
        }
        let kind = ast::ExprKind::Binary {
            op,
            lhs: Box::new(target.clone()),
            rhs: Box::new(value.clone()),
        };
        self.setter_call(obj, prop, &synth(kind, span), span)
    }

    /// `target++` / `--target` (not used as a value) through the getter and the setter.
    pub(super) fn setter_update(
        &mut self,
        obj: hir::Expr,
        op: ast::UpdateOp,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let (object, prop) = member_parts(target).expect("ICE: setter target is a member");
        let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
        if as_value {
            self.cx.err(
                format!(
                    "`{opname}` on the setter `{}` cannot be used as a value",
                    prop.name
                ),
                span,
            );
            return self.error_expr(span);
        }
        if !self.read_write_ok(obj.ty, object, prop, &format!("`{opname}`")) {
            return self.error_expr(span);
        }
        let one = synth(
            ast::ExprKind::Lit(ast::Lit::Int {
                value: 1,
                suffix: None,
            }),
            span,
        );
        let bop = if op == ast::UpdateOp::Inc {
            ast::BinaryOp::Add
        } else {
            ast::BinaryOp::Sub
        };
        let kind = ast::ExprKind::Binary {
            op: bop,
            lhs: Box::new(target.clone()),
            rhs: Box::new(one),
        };
        self.setter_call(obj, prop, &synth(kind, span), span)
    }

    /// A read-modify-write of setter `prop` needs a getter and a receiver that can be
    /// evaluated twice. Reports and returns false otherwise.
    fn read_write_ok(
        &mut self,
        t: TyId,
        object: &ast::Expr,
        prop: &ast::Ident,
        what: &str,
    ) -> bool {
        if !self.has_getter(t, &prop.name) {
            self.cx.err(
                format!("cannot read `{}`: it has a setter but no getter", prop.name),
                prop.span,
            );
            return false;
        }
        if !side_effect_free(object) {
            self.cx.err(
                format!(
                    "{what} through the setter `{}` needs a variable or field receiver",
                    prop.name
                ),
                object.span,
            );
            return false;
        }
        true
    }
}
