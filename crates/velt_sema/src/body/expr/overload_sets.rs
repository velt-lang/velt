//! Values of an overloaded function's type (`typeof f`, `crate::typeof_generic`): an object of
//! one field per signature, `#0`, `#1`, …, each holding that signature's function value.
//!
//! - `f` where its set is expected is that object: `{ #0: f#0, #1: f#1, … }`, each field the
//!   value of signature `f#k` (`crate::overloads`) at its own (generic) function type.
//! - A call through a value of the set tries the fields in order and calls the first that
//!   accepts the arguments, as a call of `f` itself does (`overload_call`); when none does it is
//!   TS2769. The call reads that one field, so it costs what a call through a field of that
//!   signature's function type costs.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};
use crate::overloads::sig_name;
use crate::typeof_generic::set_field;

impl FnCx<'_, '_> {
    /// Overloaded function `id` where its overload set type is expected: the object of its
    /// signatures' values (`None`: no set type is expected, or `id` has no signatures here).
    pub(crate) fn overload_set_value(
        &mut self,
        id: &ast::Ident,
        exp: Option<TyId>,
        want: Want,
    ) -> Option<hir::Expr> {
        let want_ty = self.hint(exp)?;
        let parts = self.cx.overload_set_parts(want_ty)?;
        let mut props = vec![];
        for k in 0..parts.len() {
            let name = sig_name(&id.name, k);
            self.cx.lookup_item_at(self.module, &name, id.span)?;
            let value = ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Ident(ast::Ident {
                    name,
                    span: id.span,
                }),
                span: id.span,
            };
            // Empty: written by the compiler (`ObjectProp` keys starting with `#` are refused).
            let key = ast::Ident {
                name: set_field(k),
                span: Span::new(id.span.file, id.span.lo, id.span.lo),
            };
            props.push(ast::ObjectProp::KeyValue(key, value));
        }
        let lit = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Object(props),
            span: id.span,
        };
        Some(self.expr(&lit, Some(want_ty), want))
    }

    /// `f(args)` (or `f<A>(args)`, `name` naming `f` in errors, empty when it has none) where `f` holds a value of an overload set type: the call of
    /// the first signature's field that accepts it; `Err(f)` when `f` is not of such a type.
    pub(crate) fn overload_set_call(
        &mut self,
        name: &str,
        f: hir::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.cx.overload_sets {
            return Err(f);
        }
        let Some(parts) = self.cx.overload_set_parts(f.ty) else {
            return Err(f);
        };
        let n = parts.len();
        let call = |s: &mut Self, k: usize| {
            let key = ast::Ident {
                name: set_field(k),
                span,
            };
            match s.field_access(f.clone(), &key, Want::Borrow, span) {
                Some(field) => s.call_value_with(field, type_args, args, exp, span),
                None => s.error_expr(span),
            }
        };
        let picked = self.pick_signature(name, n, &call);
        Ok(match picked {
            Ok(k) => call(self, k),
            Err(misfits) => self.no_overload(name, misfits, args, span),
        })
    }

    /// `recv.name(args)` where field `name` of the receiver holds an overloaded function's value,
    /// or a generic function value called with type arguments (`recv.name<A>(args)`): the call
    /// through it (`Err(recv)`: no such field).
    pub(crate) fn overload_set_field_call(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        // Only a program that wrote an overload set's type has its values; a written type argument may be for
        // a generic function value.
        if !self.cx.overload_sets && type_args.is_empty() {
            return Err(recv);
        }
        let fty = match self.field_of(recv.ty, &prop.name) {
            Some((_, t)) => Some(t),
            None => self.iface_field_of(recv.ty, &prop.name).map(|f| f.ty),
        };
        // The field call in `method_call::no_method` passes no type arguments: a generic
        // function value called with some is called here.
        let taken = |s: &mut Self, t: TyId| {
            s.cx.overload_set_parts(t).is_some()
                || (!type_args.is_empty() && s.cx.generic_sig(t).is_some())
        };
        if !fty.is_some_and(|t| taken(self, t)) {
            return Err(recv);
        }
        Ok(match self.field_access(recv, prop, Want::Borrow, span) {
            Some(f) => match self.overload_set_call(&prop.name, f, type_args, args, exp, span) {
                Ok(h) => h,
                Err(f) => self.call_value_with(f, type_args, args, exp, span),
            },
            None => self.error_expr(span),
        })
    }
}
