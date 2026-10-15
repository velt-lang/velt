//! Own keys and type tags (object_builtins.rs): `Object.hasOwn(o, k)` and
//! `Object.prototype.hasOwnProperty.call(o, k)` (a record's entry test, or whether `Object.keys(o)`
//! lists `k`: methods and accessors are on the prototype), and
//! `Object.prototype.toString.call(v)`, the `[object Tag]` string of each type `v` may have.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `Object.prototype.m` (the prelude's `Object`), as the callee of `.call`: `m`.
    pub(super) fn object_prototype_method(&mut self, e: &ast::Expr) -> Option<String> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        let ast::ExprKind::Member {
            object: o,
            prop: p,
            optional: false,
        } = &object.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(ns) = &o.kind else {
            return None;
        };
        (p.name == "prototype" && self.is_prelude_object(ns)).then(|| prop.name.clone())
    }

    /// `Object.prototype.m.call(args)`.
    pub(super) fn prototype_method_call(
        &mut self,
        m: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        match m {
            _ if self
                .dynamic_args(&format!("Object.prototype.{m}.call"), args, span)
                .is_some() =>
            {
                self.error_expr(span)
            }
            "hasOwnProperty" => self.has_own(args, "Object.prototype.hasOwnProperty.call", span),
            "toString" => self.object_to_string_call(args, span),
            _ => {
                self.cx.err(
                    format!("`Object.prototype.{m}.call` is not supported yet"),
                    span,
                );
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }

    /// `Object.hasOwn(o, k)` / `Object.prototype.hasOwnProperty.call(o, k)` (`what`).
    pub(super) fn has_own(&mut self, args: &[ast::Expr], what: &str, span: Span) -> hir::Expr {
        let [o, k] = args else {
            return self.arity_error(what, 2, args, span);
        };
        let Some(ot) = self.trial_type(o) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        if let Some((key, _)) = self.record_args(ot) {
            if key == self.cx.ty.str_ {
                let obj = self.expr(o, None, Want::Borrow);
                return self.record_call(obj, "__has", std::slice::from_ref(k), span);
            }
        }
        let object = match self.cx.ty.kind(ot) {
            TyKind::Adt(d, _) => self
                .cx
                .adt(*d)
                .is_some_and(|a| !self.cx.scopes[a.module].is_std || a.kind == AdtKind::Anon),
            TyKind::Dyn(..) => true,
            _ => false,
        };
        if !object {
            let tn = self.cx.display(ot);
            let mut d = Diagnostic::error(
                format!("`{what}` on a `{tn}` is not supported yet: it tests the own keys of an object, a class instance or a `Record<string, V>`"),
                o.span,
            );
            if self.cx.ty.opt_payload(ot).is_some() {
                d = d.with_note("test for `null` first (Node throws a `TypeError` for `null`)");
            }
            self.cx.error(d);
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        if self
            .trial_type(k)
            .is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Symbol))
        {
            self.cx.err(
                format!("`{what}` with a symbol key is not supported yet"),
                k.span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        // `Object.keys(o).includes(k)`: the built-in `Object.keys` (an empty callee span, as a
        // `for...in` loop writes it), which knows every object's own string keys.
        let empty = Span::new(span.file, span.lo, span.lo);
        let keys = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(synth(
                            ast::ExprKind::Ident(ast::Ident {
                                name: "Object".to_string(),
                                span: empty,
                            }),
                            empty,
                        )),
                        prop: ast::Ident {
                            name: "keys".to_string(),
                            span: empty,
                        },
                        optional: false,
                    },
                    empty,
                )),
                type_args: vec![],
                args: vec![o.clone()],
                optional: false,
            },
            o.span,
        );
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(keys),
                        prop: ast::Ident {
                            name: "includes".to_string(),
                            span,
                        },
                        optional: false,
                    },
                    span,
                )),
                type_args: vec![],
                args: vec![k.clone()],
                optional: false,
            },
            span,
        );
        self.expr(&call, None, Want::Move)
    }

    /// `Object.prototype.toString.call(v)`: `"[object Tag]"`.
    pub(super) fn object_to_string_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [v] = args else {
            return self.arity_error("Object.prototype.toString.call", 1, args, span);
        };
        let s = self.expr(v, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let sty = s.ty;
        let mut arms = vec![];
        for (pat, t) in self.member_patterns(sty, span) {
            let tag = match t {
                None => match self.cx.ty.nothing_of(sty) {
                    Some(hir::Nothing::Undefined) => Some("Undefined".to_string()),
                    _ => Some("Null".to_string()),
                },
                Some(t) => self.string_tag(t),
            };
            let Some(tag) = tag else {
                let tn = self.cx.display(t.unwrap_or(sty));
                self.cx.err(
                    format!("`Object.prototype.toString.call` of a `{tn}` is not supported yet"),
                    v.span,
                );
                return self.error_expr(span);
            };
            let body = self.str_lit(&format!("[object {tag}]"), span);
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let str_ = self.cx.ty.str_;
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, str_, span)
    }

    /// The `Symbol.toStringTag` JavaScript gives values of type `t` (`None`: not known).
    pub(super) fn string_tag(&mut self, t: TyId) -> Option<String> {
        let ty = &self.cx.ty;
        let tag = match ty.kind(t) {
            TyKind::Array(_) | TyKind::Tuple(_) => "Array",
            TyKind::Str | TyKind::Literal(hir::LitValue::Str(_)) => "String",
            TyKind::Bool | TyKind::Literal(hir::LitValue::Bool(_)) => "Boolean",
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Literal(_) => "Number",
            TyKind::Closure(_) | TyKind::FnPtr { .. } => "Function",
            TyKind::Promise(..) => "Promise",
            TyKind::Adt(d, _) => {
                let d = *d;
                if self.cx.union_def(t).is_some() {
                    return None;
                }
                let a = self.cx.adt(d)?;
                if a.kind == AdtKind::Anon || self.record_args(t).is_some() {
                    return Some("Object".to_string());
                }
                // The nearest class (itself or a base) that is the standard library's.
                let mut cur = Some(d);
                while let Some(c) = cur {
                    let a = self.cx.adt(c)?;
                    if a.methods.contains_key("[Symbol.toStringTag]")
                        || a.methods.contains_key("get [Symbol.toStringTag]")
                        || a.fields.iter().any(|f| f.name == "[Symbol.toStringTag]")
                    {
                        return None;
                    }
                    if self.cx.scopes[a.module].is_std {
                        let name = a.name.clone();
                        return std_string_tag(self, c, &name).map(str::to_string);
                    }
                    cur = a.base.and_then(|b| self.cx.class_of(b)).map(|(b, _)| b);
                }
                "Object"
            }
            _ => return None,
        };
        Some(tag.to_string())
    }
}

/// The `Symbol.toStringTag` of instances of standard library class `d` (`name`) or of classes
/// extending it (`None`: not known).
fn std_string_tag(s: &FnCx<'_, '_>, d: DefId, name: &str) -> Option<&'static str> {
    let is = |n: &str| s.cx.prelude_adt(n) == Some(d);
    Some(match name {
        "Map" if is("Map") => "Map",
        "WeakMap" if is("WeakMap") => "WeakMap",
        "WeakSet" if is("WeakSet") => "WeakSet",
        "Date" if is("Date") => "Date",
        "Set" => "Set",
        "RegExp" => "RegExp",
        _ if s
            .cx
            .prelude_adt("Error")
            .is_some_and(|e| s.cx.class_extends(d, e) || d == e) =>
        {
            "Error"
        }
        _ => return None,
    })
}
