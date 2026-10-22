//! Expression checking (bidirectional: `exp` is the type demanded by context, if any).
//! One submodule per concern.

mod access;
mod accessor_rmw;
mod arg_order;
mod args;
mod array_ctor;
mod assign;
mod assign_checks;
pub(crate) mod assign_paths;
mod assign_pattern;
pub(crate) use assign_pattern::REPORTED;
mod attempt;
mod await_union;
mod builtins;
mod call;
mod call_apply;
mod callables;
mod callback;
mod callback_sig;
mod callback_wrap;
mod chain;
mod closure;
mod closure_sig;
mod coerce;
mod constraints;
mod construct;
mod conversions;
mod default_args;
mod discriminated;
mod dispose_call;
mod division;
pub(crate) mod downcast;
mod dynamic;
mod dynamic_concat;
mod dynamic_member;
mod dynamic_narrow;
mod dynamic_optional;
mod dynamic_relational;
mod dynamic_whole;
mod dynamic_write;
mod env_globals;
pub(crate) use env_globals::NODE_GLOBALS;
mod error_slots;
mod error_view;
mod errors;
mod fn_union_call;
mod frozen;
mod gen_closure;
mod gen_value;
mod generic_spread;
mod hidden_nothings;
mod hoist;
pub(crate) mod iface_adapt;
mod iface_call;
mod iface_nested;
mod iface_object;
mod iface_write;
mod in_keys;
mod in_place_chain;
mod indexers;
mod int32;
mod intrinsics;
mod js_list;
mod json_layout;
pub(crate) mod jsx;
mod key_index;
mod late_let_update;
mod lit;
mod literal_keys;
mod literal_types;
mod matching;
pub(crate) mod member;
mod member_keys;
mod method;
mod method_call;
mod method_value;
mod mixed_array;
mod names;
mod namespaces;
mod narrowing_casts;
mod never_null;
mod numbers;
mod object;
mod object_assign;
pub(crate) mod object_builtins;
mod object_copy;
mod object_define;
pub(crate) mod object_keys;
mod object_method;
mod object_own;
mod object_prototypes;
mod ops;
mod optional_fn_call;
mod ordering;
mod overload_call;
mod process;
mod promise_new;
mod promise_then;
mod proxy;
mod push_items;
mod record;
mod record_call;
mod record_compound;
mod record_literal;
mod record_logical;
mod record_view;
mod regex_args;
mod rest_shapes;
mod setters;
mod simd_call;
pub(crate) mod spawn_promises;
mod spread;
mod spread_args;
mod spread_array;
mod spread_optional;
pub(crate) mod static_copy;
mod std_glue;
mod structured_clone;
mod supers;
mod symbol_keys;
mod sync;
pub(crate) mod tasks;
mod this_param;
mod this_type;
mod three_state;
mod throws_mismatch;
mod truthiness;
mod type_tests;
mod typeof_member;
mod union_coerce;
mod union_iface;
mod widen_fresh;

use velt_common::Span;
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};
pub(crate) use args::deferred;
pub(crate) use call_apply::is_member_read;
pub(crate) use callables::{property_writes, PendingWrites};

impl FnCx<'_, '_> {
    /// Check `e` against `exp`, converting (`WrapSome`/`Upcast`/`ToDyn`) or reporting a mismatch.
    pub fn expr_coerce(&mut self, e: &ast::Expr, exp: TyId, want: Want) -> hir::Expr {
        // `const f: (s: string) => void = count`: a named function adapted to the type.
        let fn_ty = self.cx.ty.core(exp).unwrap_or(exp);
        let adapted = match self.cx.ty.kind(fn_ty) {
            TyKind::FnPtr { .. } => self.callback_adapter(e, exp, callback::CallbackMode::Plain),
            _ => None,
        };
        let h = match adapted {
            Some(h) => h,
            None => self.expr(e, Some(exp), want),
        };
        if self.in_place_misuse(e, &h, exp) {
            return self.error_expr(e.span);
        }
        self.coerce(h, exp)
    }

    fn unsupported_expr(&mut self, what: &str, span: Span) -> hir::Expr {
        self.cx.err(format!("{what} are not supported yet"), span);
        self.error_expr(span)
    }

    /// `exp` with a surrounding `| null` removed (the value is wrapped after checking).
    pub fn hint(&self, exp: Option<TyId>) -> Option<TyId> {
        exp.map(|t| self.cx.ty.core(t).unwrap_or(t))
    }

    pub fn expr(&mut self, e: &ast::Expr, exp: Option<TyId>, want: Want) -> hir::Expr {
        let outer = std::mem::take(&mut self.call_temps);
        let h = self.expr_kind(e, exp, want);
        let temps = std::mem::replace(&mut self.call_temps, outer);
        let h = self.with_lets(temps, h);
        // `Object.keys(r).sort()`: the temporary array it sorted (`in_place_chain`).
        let h = self.in_place_value(e, h);
        // An integer from the standard library is a number in user code (`numbers`).
        let h = self.std_number(h);
        // An `any | null` (a generic `T | undefined` with `T = any`: `Map.get`, `find`, an
        // optional field) is an `any` in user code, as in TypeScript.
        let h = self.collapse_dynamic_option(h);
        // A generic union holding `null` / `undefined` through a member, in its written form.
        let h = self.flatten_hidden_nothings(h);
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
            A::Ident(id) if id.name == velt_syntax::ARRAY_HOLE => {
                self.cx.err(
                    "a hole in an array literal (`[1, , 2]`) is only allowed in a destructuring assignment (`[a, , b] = xs`); write `undefined`",
                    span,
                );
                self.error_expr(span)
            }
            // A property already reported (`assign_pattern::reject_cover_inits`).
            A::Ident(id) if id.name == assign_pattern::REPORTED => self.error_expr(span),
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
            A::Assign {
                op: Some(op),
                target,
                value,
            } if self.late_let_rmw(e) => self.late_let_compound(*op, target, value, exp, span),
            A::Assign { op, target, value } => self.assign_value(*op, target, value, exp, span),
            A::Update { op, prefix, target } if self.late_let_rmw(e) => {
                self.late_let_update(*op, *prefix, target, exp, span)
            }
            A::Update { op, prefix, target } => self.update(*op, *prefix, target, true, span),
            A::Cond { cond, then, els } => self.ternary(cond, then, els, exp, span),
            A::Call {
                callee,
                type_args,
                args,
                optional,
            } if !*optional && args.iter().any(|a| matches!(a.kind, A::Spread(_))) => {
                self.call_spreading(callee, type_args, args, exp, span)
            }
            // A destructuring default (`pattern_defaults::undefined_or`): applies on `undefined`.
            A::Call { callee, args, .. }
                if args.len() == 2
                    && matches!(&callee.kind, A::Ident(i) if i.name == super::pattern_defaults::UNDEFINED_OR) =>
            {
                self.undefined_or(&args[0], &args[1], exp, span)
            }
            // `Object.assign` onto a variable or field path gives that place itself, used as
            // the consumer wants it (`const r = Object.assign(t, s)` names `t`'s object).
            A::Call {
                callee,
                type_args,
                args,
                optional: false,
            } if self.object_assign_callee(callee) => match self.callable_assign(e, exp, span) {
                Some(h) => h,
                None => self.object_assign_call(type_args, args, want, span),
            },
            A::Call {
                callee,
                type_args,
                args,
                optional,
            } => {
                let call = self.call(callee, type_args, args, *optional, exp, span);
                self.predicate_checked(call, span)
            }
            A::New { class, args } => self.new_expr(class, args, exp, span),
            A::Member {
                object,
                prop,
                optional,
            } => match self.prototype_read(object, prop, *optional, span) {
                Some(h) => h,
                None => self.member(object, prop, *optional, exp, want, span),
            },
            A::Index {
                object,
                index,
                optional,
            } => self.index_expr(object, index, *optional, exp, want, span),
            A::Arrow { .. } => self.closure(e, exp, true),
            A::Function(d) => self.function_expr(d, exp, span),
            A::Array(elems) => self.array_lit(elems, exp, span),
            A::Object(props) => match self.reject_cover_inits(props) {
                Some(props) => self.object_lit(&props, exp, span),
                None => self.object_lit(props, exp, span),
            },
            A::StructLit { name, props } => self.struct_lit(name, props, exp, span),
            A::Spread(_) => self.unsupported_expr("spread arguments (`f(...xs)`)", span),
            A::Await(inner) => self.await_expr(inner, exp, span),
            A::Yield { arg, delegate } => self.yield_expr(arg.as_deref(), *delegate, span),
            // `xs as const`: TS narrows the type to literals and `readonly`; the value is the
            // same (`readonly_arrays::as_const`).
            A::Cast { expr, ty } if member::is_as_const(ty) => self.as_const(expr, exp, want),
            A::Cast { expr, ty } => self.cast(expr, ty, want, span),
            A::InstanceOf { expr, ty } => self.instanceof(expr, ty, span),
            A::Paren(inner) => self.expr(inner, exp, want),
            A::NonNull(inner) => self.non_null(inner, exp, want, span),
            A::Jsx(el) => self.jsx(el),
        }
    }
}
