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
        if m == "hasOwnProperty" {
            if let Some(h) = self.any_has_own(args, span) {
                return h;
            }
        }
        if m == "toString" && self.first_arg_dynamic(args) {
            return self.object_to_string_call(args, span);
        }
        match m {
            _ if self
                .reject_dynamic_args(&format!("Object.prototype.{m}.call"), args, span)
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

    /// Is the first of `args` an `any` or an `object` (not `unknown`)?
    pub(super) fn first_arg_dynamic(&mut self, args: &[ast::Expr]) -> bool {
        args.first()
            .and_then(|a| self.trial_type(a))
            .is_some_and(|t| self.spreads_dynamic(t))
    }

    /// `Object.hasOwn(o, k)` / `Object.prototype.hasOwnProperty.call(o, k)` of an `any` or
    /// `object` `o` with a `string`, `symbol` or `string | symbol` key: the prelude's
    /// `__anyHasOwn(o, k)`, which asks the value held (`None`: `o` is not one of them).
    pub(super) fn any_has_own(&mut self, args: &[ast::Expr], span: Span) -> Option<hir::Expr> {
        let [o, k] = args else {
            return None;
        };
        let ot = self.trial_type(o)?;
        let ot = self.cx.ty.core(ot).unwrap_or(ot);
        if !self.spreads_dynamic(ot) {
            return None;
        }
        let kt = self.trial_type(k)?;
        // A `string | symbol` key (a proxy trap's) or an `any` one is checked when it arrives.
        let wk = self.cx.widened(kt);
        let keyed = wk == self.cx.ty.str_
            || self.string_or_symbol(kt)
            || matches!(self.cx.ty.kind(wk), TyKind::Symbol)
            || self.cx.dynamic(kt) == Some(crate::any::Dynamic::Any);
        if !keyed {
            return None;
        }
        // The keys are listed through the value's descriptor (a record's by its methods).
        self.note_dyn_members();
        // A symbol constant names the member it keys, as in `KEY in o`; a proxy's trap sees the
        // symbol itself.
        if let Some(name) = self.symbol_key_of(k).filter(|_| self.uses_proxy()) {
            let name = synth(ast::ExprKind::Lit(ast::Lit::Str(name)), k.span);
            let args = vec![o.clone(), name, k.clone()];
            return Some(self.prelude_fn_call("__anyHasOwnSym", args, span));
        }
        let f = self.proxy_op("__anyHasOwn");
        Some(self.prelude_fn_call(f, vec![o.clone(), k.clone()], span))
    }

    /// `Object.hasOwn(o, k)` / `Object.prototype.hasOwnProperty.call(o, k)` (`what`).
    pub(super) fn has_own(&mut self, args: &[ast::Expr], what: &str, span: Span) -> hir::Expr {
        let [o, k] = args else {
            return self.arity_error(what, 2, args, span);
        };
        if self.typed_proxy_once(o) {
            // `Object.hasOwn(f(), k)` of a typed proxy: the operand once, then as below.
            return self.operand_once(o, span, |cx, t| {
                cx.has_own(&[t.clone(), k.clone()], what, span)
            });
        }
        let Some(ot) = self.trial_type(o) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        if let Some((key, _)) = self.record_args(ot) {
            if key == self.cx.ty.str_ {
                // A symbol or `string | symbol` key: the symbol table too.
                let symbolic = self
                    .trial_type(k)
                    .is_some_and(|t| self.symbolic_key_type(t));
                let keyed = symbolic || self.cx.objects.record_accessors;
                let method = if keyed { "__hasKeyed" } else { "__has" };
                let obj = self.expr(o, None, Want::Borrow);
                return self.record_call(obj, method, std::slice::from_ref(k), span);
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
        if let Some(h) = self.proxy_has_own(o, k, &call, span) {
            return h;
        }
        self.expr(&call, None, Want::Move)
    }

    /// A call of the prelude function `f` with the expressions `args`, checked.
    pub(crate) fn prelude_fn_call(
        &mut self,
        f: &str,
        args: Vec<ast::Expr>,
        span: Span,
    ) -> hir::Expr {
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Ident(ast::Ident {
                        name: f.to_string(),
                        span,
                    }),
                    span,
                )),
                type_args: vec![],
                args,
                optional: false,
            },
            span,
        );
        self.expr(&call, None, Want::Move)
    }

    /// What the value of the `any` (or `object`) `s` is, by its class: `match (s) {
    /// InstanceOf(C) => class(C, s), ..., _ => <fallback>(s) }` over every class of the program
    /// (the standard library's too), deepest first, of type `ret`; `fallback` is a prelude
    /// function taking the `any`, for values that are no class instances.
    pub(super) fn dynamic_class_match(
        &mut self,
        s: hir::Expr,
        ret: TyId,
        fallback: &str,
        class: impl FnMut(&mut Self, DefId, ast::Expr) -> hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let fallback = (fallback, vec![]);
        self.dynamic_class_match_where(s, ret, fallback, |_, _| true, class, span)
    }

    /// [`Self::dynamic_class_match`] over the classes `only` keeps; the others go to `fallback`
    /// (the prelude function, called with the value and the arguments given with it).
    pub(super) fn dynamic_class_match_where(
        &mut self,
        s: hir::Expr,
        ret: TyId,
        fallback: (&str, Vec<ast::Expr>),
        only: impl Fn(&mut Self, DefId) -> bool,
        mut class: impl FnMut(&mut Self, DefId, ast::Expr) -> hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let sty = s.ty;
        let (l, mode) = self.option_binding(&s, sty, "<dyn>", false);
        let name = format!("<dyn@{}>", span.lo);
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name.clone(), l);
        }
        let read = synth(ast::ExprKind::Ident(ast::Ident { name, span }), span);
        let mut classes: Vec<(usize, DefId)> = (0..self.cx.info.len() as u32)
            .map(DefId)
            .filter(|&d| {
                self.cx
                    .adt(d)
                    .is_some_and(|a| a.kind == AdtKind::Class && !is_internal(&a.name))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|&d| only(self, d))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|d| (self.class_depth(d), d))
            .collect();
        classes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1 .0.cmp(&b.1 .0)));
        let mut arms = vec![];
        for (_, c) in classes {
            let body = class(self, c, read.clone());
            arms.push(hir::Arm {
                pat: self.pat(hir::PatKind::InstanceOf(c), sty, span),
                guard: None,
                body,
            });
        }
        // The value's members are read through its descriptor.
        self.note_dyn_members();
        let (fallback, more) = fallback;
        let args = std::iter::once(read).chain(more).collect();
        let body = self.prelude_fn_call(fallback, args, span);
        arms.push(hir::Arm {
            pat: self.pat(hir::PatKind::Wildcard, sty, span),
            guard: None,
            body,
        });
        let inner = self.mk(H::Local(l, hir::UseMode::Borrow), sty, span);
        let m = self.mk(
            H::Match {
                scrutinee: Box::new(inner),
                arms,
            },
            ret,
            span,
        );
        let pat = self.pat(hir::PatKind::Binding(l, mode), sty, span);
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms: vec![hir::Arm {
                pat,
                guard: None,
                body: m,
            }],
        };
        self.mk(kind, ret, span)
    }

    /// `Object.prototype.toString.call(v)` of an `any` or `object` `v`: the tag of its class,
    /// else the prelude's `__anyObjectTag(v)` (primitives, arrays, objects).
    fn dynamic_to_string(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        self.dynamic_class_match(
            s,
            str_,
            "__anyObjectTag",
            |lw, c, _| {
                let n = lw.cx.adt(c).map_or(0, |a| a.generics.len());
                let params: Vec<TyId> = (0..n as u32).map(|i| lw.cx.ty.param(i)).collect();
                let t = lw.cx.ty.intern(TyKind::Adt(c, params));
                match lw.string_tag(t) {
                    Some(tag) => lw.str_lit(&format!("[object {tag}]"), span),
                    None => {
                        let name = lw.cx.adt(c).map_or(String::new(), |a| a.name.clone());
                        let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(name)), span);
                        lw.prelude_fn_call("__anyTagUnsupported", vec![lit], span)
                    }
                }
            },
            span,
        )
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
        if self.spreads_dynamic(sty) && self.cx.ty.opt_payload(sty).is_none() {
            return self.dynamic_to_string(s, span);
        }
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

/// A prelude class whose objects never reach an `any`: an array view's state
/// (std/prelude/zzz_adapt.vlt; a view converts to `any` as the array it views).
fn is_internal(name: &str) -> bool {
    matches!(
        name.rsplit("::").next(),
        Some("__ArrayView" | "__ClassMarker")
    )
}
