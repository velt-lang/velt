//! Expression checking (bidirectional: `exp` is the type demanded by context, if any).
//! One submodule per concern.

mod access;
mod args;
mod array_ctor;
mod assign;
mod attempt;
mod builtins;
mod call;
mod chain;
mod closure;
mod closure_sig;
mod coerce;
mod construct;
mod discriminated;
mod dispose_call;
mod errors;
mod iface_call;
mod intrinsics;
pub(crate) mod jsx;
mod lit;
mod literal_types;
mod matching;
mod member;
mod method;
mod method_call;
mod names;
mod namespaces;
mod numbers;
mod object;
mod ops;
mod ordering;
mod process;
mod promise_new;
mod promise_reads;
mod record;
mod record_call;
mod setters;
mod spread;
mod std_glue;
mod supers;
mod sync;
mod tasks;
mod truthiness;
mod type_tests;
mod union_coerce;

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, TyId};

impl FnCx<'_, '_> {
    /// Check `e` against `exp`, converting (`WrapSome`/`Upcast`/`ToDyn`) or reporting a mismatch.
    pub fn expr_coerce(&mut self, e: &ast::Expr, exp: TyId, want: Want) -> hir::Expr {
        let h = self.expr(e, Some(exp), want);
        self.coerce(h, exp)
    }

    fn unsupported_expr(&mut self, what: &str, span: Span) -> hir::Expr {
        self.cx.err(format!("{what} are not supported yet"), span);
        self.error_expr(span)
    }

    /// `exp` with a surrounding `| null` removed (the value is wrapped after checking).
    pub fn hint(&self, exp: Option<TyId>) -> Option<TyId> {
        exp.map(|t| self.cx.ty.opt_payload(t).unwrap_or(t))
    }

    pub fn expr(&mut self, e: &ast::Expr, exp: Option<TyId>, want: Want) -> hir::Expr {
        let h = self.expr_kind(e, exp, want);
        if self.cx.recording() {
            // Function values show their parameter names (`(x: i64) => string`).
            let shown = self.shown_ty(&h);
            self.cx.rec_ty(e.span, shown);
        }
        h
    }

    fn expr_kind(&mut self, e: &ast::Expr, exp: Option<TyId>, want: Want) -> hir::Expr {
        use ast::ExprKind as A;
        let span = e.span;
        if let Some(h) = self.literal_typed(e, exp) {
            return h;
        }
        if let Some(h) = self.short_circuit(e, exp, want) {
            return h;
        }
        match &e.kind {
            A::Lit(l) => self.lit(l, exp, span, false),
            A::Template { quasis, exprs } => self.template(quasis, exprs, span),
            A::Ident(id) => self.ident_expr(id, exp, want),
            A::This => self.this_expr(want, span),
            A::Super => {
                self.cx.err(
                    "`super` can only be used as `super(...)` in a constructor or `super.method(...)`",
                    span,
                );
                self.error_expr(span)
            }
            A::Unary { op, expr } => self.unary(*op, expr, exp, span),
            A::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, exp, span),
            A::Assign { op, target, value } => self.assign_value(*op, target, value, exp, span),
            A::Update { op, prefix, target } => self.update(*op, *prefix, target, true, span),
            A::Cond { cond, then, els } => self.ternary(cond, then, els, exp, span),
            A::Call {
                callee,
                type_args,
                args,
                optional,
            } => self.call(callee, type_args, args, *optional, exp, span),
            A::New { class, args } => self.new_expr(class, args, exp, span),
            A::Member {
                object,
                prop,
                optional,
            } => self.member(object, prop, *optional, exp, want, span),
            A::Index {
                object,
                index,
                optional,
            } => self.index_expr(object, index, *optional, want, span),
            A::Arrow { .. } => self.closure(e, exp, true),
            A::Array(elems) => self.array_lit(elems, exp, span),
            A::Object(props) => self.object_lit(props, exp, span),
            A::StructLit { name, props } => self.struct_lit(name, props, exp, span),
            A::Spread(_) => self.unsupported_expr("spread arguments (`f(...xs)`)", span),
            A::Await(inner) => self.await_expr(inner, exp, span),
            A::Cast { expr, ty } => self.cast(expr, ty, span),
            A::InstanceOf { expr, ty } => self.instanceof(expr, ty, span),
            A::Paren(inner) => self.expr(inner, exp, want),
            A::Jsx(el) => self.jsx(el),
        }
    }
}
