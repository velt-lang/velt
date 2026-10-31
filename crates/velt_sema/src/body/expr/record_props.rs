//! Property operations on a string-keyed record (std/prelude/record.vlt), which holds its
//! symbol keys in a table of their own and the accessors `Object.defineProperty` defines in
//! another, as Node keeps them:
//!
//! - `Object.defineProperty(r, key, { get, set, configurable })`: `r.__defineAccessor(...)`, an
//!   accessor that is not enumerable (`Object.keys`, `JSON.stringify`, spreads and
//!   `console.log` skip it). Every record operation of a program that defines accessors goes
//!   through the `Keyed` methods, which look at them (`Ctx::objects.record_accessors`).
//! - `Object.defineProperty(r, key, { value })` with every attribute `true`: a write of the
//!   entry, the attributes every entry has.
//! - `Object.getOwnPropertySymbols(r)`: the symbol keys in insertion order.
//! - `Reflect.ownKeys(r)`: the string keys in `Object.keys` order, then the symbol keys.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::object_builtins::strip_parens;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir;

impl FnCx<'_, '_> {
    /// Is `e` a string-keyed record (checked on trial)?
    fn string_record(&mut self, e: &ast::Expr) -> bool {
        if !self.has_record_method("__ownKeys") {
            return false;
        }
        let str_ = self.cx.ty.str_;
        self.trial_type(e)
            .and_then(|t| self.record_args(t))
            .is_some_and(|(k, _)| k == str_)
    }

    /// `Object.defineProperty(r, key, desc)` on a string-keyed record (`None`: another target).
    pub(super) fn record_define_property(
        &mut self,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let [o, key, desc] = args else {
            return None;
        };
        if !self.string_record(o) {
            return None;
        }
        let ast::ExprKind::Object(props) = &strip_parens(desc).kind else {
            self.cx.err(
                "`Object.defineProperty` with a descriptor that is not an object literal is not supported yet",
                desc.span,
            );
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        };
        let (mut get, mut set, mut value) = (None, None, None);
        let mut flags = [None; 3];
        for p in props {
            let (name, v) = match p {
                ast::ObjectProp::KeyValue(n, v) => (n, v.clone()),
                ast::ObjectProp::Shorthand(n) => {
                    (n, synth(ast::ExprKind::Ident(n.clone()), n.span))
                }
                _ => {
                    self.cx
                        .err("this property descriptor is not supported yet", desc.span);
                    self.check_args_loose(args);
                    return Some(self.error_expr(span));
                }
            };
            let flag = |s: &str| {
                ["enumerable", "writable", "configurable"]
                    .iter()
                    .position(|f| *f == s)
            };
            match name.name.as_str() {
                "get" => get = Some(v),
                "set" => set = Some(v),
                "value" => value = Some(v),
                n if flag(n).is_some() => {
                    let ast::ExprKind::Lit(ast::Lit::Bool(b)) = strip_parens(&v).kind else {
                        self.cx.err(
                            format!("`{n}` in a property descriptor must be `true` or `false` while compiling"),
                            v.span,
                        );
                        self.check_args_loose(args);
                        return Some(self.error_expr(span));
                    };
                    flags[flag(n).unwrap_or(0)] = Some(b);
                }
                other => {
                    self.cx.err(
                        format!("`{other}` is not a property descriptor attribute (`value`, `enumerable`, `writable`, `configurable`, `get`, `set`)"),
                        name.span,
                    );
                    self.check_args_loose(args);
                    return Some(self.error_expr(span));
                }
            }
        }
        let [enumerable, writable, configurable] = flags;
        let accessor = get.is_some() || set.is_some();
        let unsupported = match accessor {
            true if value.is_some() || writable.is_some() => Some(
                "a property descriptor with both an accessor and a `value` or `writable` (Node's `TypeError`)",
            ),
            true if enumerable == Some(true) => {
                Some("an enumerable accessor on a record is not supported yet")
            }
            false if [enumerable, writable, configurable] != [Some(true); 3] => Some(
                "`Object.defineProperty` of a data property on a record needs `enumerable`, `writable` and `configurable` all `true` (a record's entries have them); other attributes are not supported yet",
            ),
            _ => None,
        };
        if let Some(msg) = unsupported {
            self.cx.error(Diagnostic::error(msg.to_string(), desc.span));
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        }
        let obj = self.expr(o, None, Want::Borrow);
        let call = if accessor {
            let conf = synth(
                ast::ExprKind::Lit(ast::Lit::Bool(configurable == Some(true))),
                span,
            );
            // The getter and setter may throw (std/lang/record/symbols.vlt): each is a call's own
            // argument, so what it throws is inferred from it.
            let (method, call_args) = match (get, set) {
                (Some(g), Some(s)) => ("__defineAccessor", vec![key.clone(), g, s, conf]),
                (Some(g), None) => ("__defineGetter", vec![key.clone(), g, conf]),
                (None, Some(s)) => ("__defineSetter", vec![key.clone(), s, conf]),
                (None, None) => unreachable!("ICE: an accessor descriptor without an accessor"),
            };
            self.record_call(obj, method, &call_args, span)
        } else {
            let v = value.unwrap_or_else(|| synth(ast::ExprKind::Ident(undefined(span)), span));
            self.record_call(obj, "__setKeyed", &[key.clone(), v], span)
        };
        Some(call)
    }

    /// `Object.getOwnPropertyDescriptor(r, k)` on a string-keyed record: its property `k` as
    /// Node describes it (std/lang/record/reflect.vlt), or `undefined`.
    pub(super) fn record_descriptor(
        &mut self,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let [r, k] = args else {
            return None;
        };
        if !self.has_record_method("__descriptor") || !self.string_record(r) {
            return None;
        }
        let obj = self.expr(r, None, Want::Borrow);
        Some(self.record_call(obj, "__descriptor", std::slice::from_ref(k), span))
    }

    /// `Object.getOwnPropertySymbols(r)` / `Reflect.ownKeys(r)` (`method`: `__ownSymbols`,
    /// `__ownKeys`) on a string-keyed record (`None`: another argument).
    pub(super) fn record_own_keys(
        &mut self,
        args: &[ast::Expr],
        method: &str,
        span: Span,
    ) -> Option<hir::Expr> {
        let [r] = args else {
            return None;
        };
        if !self.string_record(r) {
            return None;
        }
        let obj = self.expr(r, None, Want::Borrow);
        Some(self.record_call(obj, method, &[], span))
    }
}

fn undefined(span: Span) -> ast::Ident {
    ast::Ident {
        name: "undefined".to_string(),
        span,
    }
}
