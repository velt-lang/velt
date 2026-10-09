//! Setters (`set name(v: T) { ... }`, docs/reference/classes.md): `x.name = v` on a value
//! whose type has a setter `name` (and no field of that name) is the call `x.set name(v)` —
//! setters live under their own method-table key ([`member_key`]), so a getter and a setter
//! may share a name. Compound assignment and `++`/`--` read through the getter and write
//! through the setter, one after the other ([`accessor_rmw`](super::accessor_rmw)).

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::Rmw;
use super::method::Resolved;
use crate::body::FnCx;
use crate::defs::member_key;
use crate::hir::{self, TyId};

/// `(object, prop)` of an assignment target `object.prop` or `object["prop"]` (parentheses
/// removed).
fn member_parts(target: &ast::Expr) -> Option<(&ast::Expr, ast::Ident)> {
    match &target.kind {
        ast::ExprKind::Paren(inner) => member_parts(inner),
        _ => super::member::member_view(target),
    }
}

/// Evaluating `e` twice is the same as once: a variable, `this` or a field path of those.
pub(super) fn side_effect_free(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(x) => side_effect_free(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => side_effect_free(object),
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } if super::member::literal_key(index).is_some() => side_effect_free(object),
        _ => false,
    }
}

pub(super) fn synth(kind: ast::ExprKind, span: Span) -> ast::Expr {
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

    /// The parameter type of setter `name` of type `t` (`None` when it is not known here).
    pub(super) fn setter_param_ty(&mut self, t: TyId, name: &str) -> Option<TyId> {
        let (ty, slots) = match self.resolve_method(t, &member_key(name, true))? {
            Resolved::Def { def, slots, .. } | Resolved::Virtual { def, slots, .. } => {
                (self.cx.fn_info(def).params.first()?.ty, slots)
            }
            Resolved::Iface {
                method, iface_args, ..
            } => {
                let slots = iface_args.into_iter().map(Some).collect();
                (method.params.first()?.ty, slots)
            }
            Resolved::Builtin(_) => return None,
        };
        let ty = self.cx.subst_known(ty, &slots);
        (!self.cx.ty.has_error(ty)).then_some(ty)
    }

    /// `obj.prop = arg` through the setter (`obj` is the checked receiver).
    pub(super) fn setter_call(
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
    /// `as_value`: the assignment's value is used.
    pub(super) fn setter_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let (object, prop) = member_parts(target).expect("ICE: setter target is a member");
        let prop = &prop;
        if self.private_accessor_outside(obj.ty, prop, true) {
            self.check_args_loose(std::slice::from_ref(value));
            return self.error_expr(span);
        }
        let Some(op) = op else {
            if as_value {
                return self.accessor_assign_value(obj, object, prop, value, span);
            }
            return self.setter_call(obj, prop, value, span);
        };
        if !self.readable(obj.ty, prop) {
            self.check_args_loose(std::slice::from_ref(value));
            return self.error_expr(span);
        }
        let rmw = match op {
            ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish => {
                Rmw::Logical(op, value)
            }
            _ => Rmw::Compound(op, value),
        };
        self.accessor_rmw(obj, object, prop, rmw, as_value, span)
    }

    /// `target++` / `--target` through the getter and the setter.
    pub(super) fn setter_update(
        &mut self,
        obj: hir::Expr,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let (object, prop) = member_parts(target).expect("ICE: setter target is a member");
        let prop = &prop;
        if self.private_accessor_outside(obj.ty, prop, true) || !self.readable(obj.ty, prop) {
            return self.error_expr(span);
        }
        let rmw = Rmw::Update(op, prefix);
        self.accessor_rmw(obj, object, prop, rmw, as_value, span)
    }

    /// A read-modify-write of setter `prop` needs a getter. Reports and returns false otherwise.
    fn readable(&mut self, t: TyId, prop: &ast::Ident) -> bool {
        if self.has_getter(t, &prop.name) {
            return true;
        }
        self.cx.err(
            format!("cannot read `{}`: it has a setter but no getter", prop.name),
            prop.span,
        );
        false
    }
}
