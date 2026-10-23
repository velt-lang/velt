//! `x.constructor.name` and `x.constructor?.name` of an `any`, `object` or `Function` `x`
//! (issues #924, #926): the name of the constructor of the value held, as JavaScript gives it.
//! A class instance gives its class's name (a subclass's when it was converted through its
//! base); an object literal or a record `"Object"`, an array `"Array"`, a primitive its
//! wrapper's name (`"Number"`); the prelude's `__anyCtorName` answers what is not a class
//! instance. `x.constructor` as a value (a class object) is not supported yet.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, DefId};

/// The standard library's classes whose instances JavaScript gives a constructor of the same
/// name.
const STD_CTORS: &[&str] = &[
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "WeakRef",
    "Date",
    "RegExp",
    "Promise",
    "URL",
    "URLSearchParams",
    "TextEncoder",
    "TextDecoder",
    "ArrayBuffer",
    "DataView",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float32Array",
    "Float64Array",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
    "AggregateError",
];

impl FnCx<'_, '_> {
    /// `X.constructor.name` / `X.constructor?.name` where `X` is an `any`, `object` or
    /// `Function` (`None`: another member read, checked as usual).
    pub(crate) fn ctor_name_read(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        if prop.name != "name" {
            return None;
        }
        let ast::ExprKind::Member {
            object: inner,
            prop: ctor,
            optional: false,
        } = &strip(object).kind
        else {
            return None;
        };
        if ctor.name != "constructor" || !self.ctor_receiver(inner) {
            return None;
        }
        let s = self.expr(inner, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return Some(self.error_expr(span));
        }
        Some(self.dynamic_ctor_name(s, span))
    }

    /// Is `e` (checked an extra time when it is a path) an `any`, `object` or `Function`? A
    /// cast is answered by its written type.
    fn ctor_receiver(&mut self, e: &ast::Expr) -> bool {
        let e = strip(e);
        let t = match &e.kind {
            ast::ExprKind::Cast { ty, .. } => {
                // Resolved again where the cast is checked: its errors are reported there.
                let mark = self.cx.diags.len();
                let t = self.resolve(ty);
                self.cx.diags.truncate(mark);
                Some(t)
            }
            _ => self.peek_ty(e),
        };
        t.is_some_and(|t| {
            matches!(
                self.cx.dynamic(t),
                Some(Dynamic::Any | Dynamic::Object | Dynamic::Function)
            )
        })
    }

    /// The constructor's name of the value of the dynamic `s`, by its class.
    fn dynamic_ctor_name(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        self.dynamic_class_match(
            s,
            str_,
            "__anyCtorName",
            |lw, c, _| match lw.class_ctor_name(c) {
                Some(n) => lw.str_lit(&n, span),
                None => {
                    let name = lw.cx.adt(c).map_or(String::new(), |a| a.name.clone());
                    let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(name)), span);
                    lw.prelude_fn_call("__anyCtorNameUnsupported", vec![lit], span)
                }
            },
            span,
        )
    }

    /// The name JavaScript gives the constructor of class `c`'s instances (`None`: a class of
    /// the standard library Velt has no JavaScript name for).
    fn class_ctor_name(&mut self, c: DefId) -> Option<String> {
        let a = self.cx.adt(c)?;
        if a.kind != AdtKind::Class {
            return None;
        }
        let name = a.name.clone();
        if !self.cx.scopes[a.module].is_std {
            return Some(name.rsplit('.').next().unwrap_or(&name).to_string());
        }
        if self.record_def(c) {
            return Some("Object".to_string());
        }
        STD_CTORS.contains(&name.as_str()).then_some(name)
    }

    /// Is `c` the prelude's `Record`?
    fn record_def(&self, c: DefId) -> bool {
        self.cx.prelude_adt("Record") == Some(c)
    }
}

/// `e` without parentheses.
fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip(inner),
        _ => e,
    }
}
