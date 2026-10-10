//! Component elements (`<Card …>`, `<ui.Card …>`): the tag names a function
//! `(props: P) => …` that is passed **uncalled** with its props, exactly like TypeScript's
//! `jsx(Card, props)`: `jsxComponent(component, props, key, name)` (or `jsxAsyncComponent` when
//! it returns a promise). The runtime decides when the component runs (parent-first rendering,
//! context). The component's type is checked against the runtime's parameter type, so a runtime
//! may accept components returning more than `JSX.Element`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::props::{ElementProps, ExtraAttr};
use super::provider::Provider;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::defs::FnKind;
use crate::hir::{self, DefId, TyId, TyKind, UseMode};
use crate::ide::record::Target;

/// The function a component tag denotes.
pub(super) enum ComponentFn {
    /// A named function (`Card`, `ui.Card`), written as `callee`.
    Named {
        def: DefId,
        callee: ast::Expr,
        takes_props: bool,
    },
    /// Any other function value (a local arrow function, …), already evaluated.
    Value(hir::Expr),
}

/// A resolved component.
pub(super) struct Component {
    pub func: ComponentFn,
    /// The props type `P` (`{}` for a function without parameters); it mentions the component's
    /// own type parameters (`slot_names`) for a generic component.
    pub props: TyId,
    /// The component's result type (in terms of its type parameters).
    pub ret: TyId,
    pub slot_names: Vec<String>,
    pub is_async: bool,
    /// Stable identity for hydration markers: `"<module path>#<Name>"`.
    pub identity: String,
}

impl FnCx<'_, '_> {
    /// `<Name …>…</Name>` → `jsxComponent(Name, props, key, identity)` (or `jsxAsyncComponent`).
    /// The attributes are evaluated in source order, then the children, then the key
    /// (`component_props`); the component runs when the runtime calls it.
    pub(super) fn jsx_component(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        name: &ast::JsxName,
    ) -> hir::Expr {
        let tag = name.to_source();
        let resolved = self.component(name, &tag).and_then(|c| {
            let rt = self.component_runtime_fn(p, &c, &tag, name.span())?;
            Some((c, rt))
        });
        let Some((c, (d, f))) = resolved else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        let mut lets = vec![];
        let Some(ElementProps {
            props,
            type_args,
            key,
            extra,
        }) = self.component_props(p, el, &c, &tag, &mut lets)
        else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        if type_args.contains(&self.cx.ty.error) {
            return self.error_expr(el.span);
        }
        // With attributes from `JSX.IntrinsicAttributes` the element calls
        // `jsxComponentAttributes`, whose component parameter gives the expected type.
        let (d, f) = match (&p.component_attrs, extra.is_empty()) {
            (Some(attrs), false) => {
                if c.is_async {
                    let span = extra[0].span;
                    self.cx.error(
                        Diagnostic::error(
                            format!("`{}` is not supported on an async component (<{tag}>)", extra[0].name),
                            span,
                        )
                        .with_note(format!(
                            "the JSX provider '{}' receives it through `jsxComponentAttributes`, which takes a synchronous component",
                            p.source
                        )),
                    );
                    return self.error_expr(el.span);
                }
                (attrs.call, "jsxComponentAttributes")
            }
            _ => (d, f),
        };
        let ret = self.cx.subst(c.ret, &type_args);
        let natural = self.cx.ty.fn_ptr(vec![props.ty], ret);
        let expected = self.runtime_component_type(d, props.ty, natural);
        if matches!(c.func, ComponentFn::Named { .. }) && !self.result_fits(natural, expected) {
            let (n, e) = (self.cx.display(natural), self.cx.display(expected));
            let why = format!(
                "its type `{n}` is not assignable to `{e}`, the component type of the JSX provider"
            );
            self.not_component(&tag, name.span(), why);
            return self.error_expr(el.span);
        }
        let Some(value) = self.component_value(c.func, expected, props.ty, el, &tag) else {
            return self.error_expr(el.span);
        };
        let identity = self.str_lit(&c.identity, name.span());
        let mut args = vec![value, props, key, identity];
        if !extra.is_empty() {
            let (names, values) = self.attr_arrays(p, extra, el.span, &mut lets);
            args.push(names);
            args.push(values);
        }
        let call = self.jsx_call(p, d, f, args, el.span);
        self.with_lets(lets, call)
    }

    /// `names` and `values` of the attributes from `JSX.IntrinsicAttributes`, in source order:
    /// array literals, or arrays filled by statements in `lets` when an attribute is passed only
    /// if it is not null (then every value is a constant or a temporary).
    fn attr_arrays(
        &mut self,
        p: &Provider,
        extra: Vec<ExtraAttr>,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> (hir::Expr, hir::Expr) {
        let str_ = self.cx.ty.str_;
        let names_ty = self.cx.ty.array(str_);
        let values_ty = self.cx.ty.array(p.attr_value);
        if !extra.iter().any(|e| e.absent_if_null) {
            let mut names = vec![];
            let mut values = vec![];
            for e in extra {
                names.push(self.str_lit(&e.name, e.span));
                values.push(e.value);
            }
            let names = self.mk(hir::ExprKind::ArrayLit(names), names_ty, span);
            let values = self.mk(hir::ExprKind::ArrayLit(values), values_ty, span);
            return (names, values);
        }
        let names_l = self.filled_array("<names>", names_ty, span, lets);
        let values_l = self.filled_array("<values>", values_ty, span, lets);
        let note = format!(
            "attributes of `JSX.IntrinsicAttributes` are passed to the JSX provider '{}' as `JSX.AttrValue`",
            p.source
        );
        for e in extra {
            let name = self.str_lit(&e.name, e.span);
            if !e.absent_if_null {
                let pushes = vec![
                    self.push_to(names_l, names_ty, name),
                    self.push_to(values_l, values_ty, e.value),
                ];
                lets.extend(pushes);
                continue;
            }
            // `if (v != null) { names.push(n); values.push(v); }`
            let mut s = e.value;
            let payload = self
                .cx
                .ty
                .opt_payload(s.ty)
                .expect("ICE: an attribute that may be absent is nullable");
            let (l, mode) = self.option_binding(&s, payload, "<attr>", true);
            if mode == UseMode::Move {
                self.force_move(&mut s);
            }
            let v = self.mk(hir::ExprKind::Local(l, mode), payload, e.span);
            let v = self.jsx_coerce(v, p.attr_value, &note);
            let stmts = vec![
                self.push_to(names_l, names_ty, name),
                self.push_to(values_l, values_ty, v),
            ];
            let unit = self.cx.ty.unit;
            let body = hir::Block {
                stmts,
                value: None,
                span: e.span,
            };
            let body = self.mk(hir::ExprKind::Block(body), unit, e.span);
            let none = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), unit, e.span);
            let sty = s.ty;
            let some = self.pat(hir::PatKind::Binding(l, mode), payload, e.span);
            let arms = vec![
                hir::Arm {
                    pat: self.pat(hir::PatKind::Some(Box::new(some)), sty, e.span),
                    guard: None,
                    body,
                },
                hir::Arm {
                    pat: self.pat(hir::PatKind::None, sty, e.span),
                    guard: None,
                    body: none,
                },
            ];
            let kind = hir::ExprKind::Match {
                scrutinee: Box::new(s),
                arms,
            };
            let m = self.mk(kind, unit, e.span);
            lets.push(hir::Stmt {
                kind: hir::StmtKind::Expr(m),
                span: e.span,
            });
        }
        let names = self.mk(hir::ExprKind::Local(names_l, UseMode::Move), names_ty, span);
        let values = self.mk(
            hir::ExprKind::Local(values_l, UseMode::Move),
            values_ty,
            span,
        );
        (names, values)
    }

    /// `let <name>: ty = [];` (mutable) in `lets`.
    fn filled_array(
        &mut self,
        name: &str,
        ty: TyId,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::LocalId {
        let l = self.new_local(name, ty, true, span, LocalKind::Temp);
        let init = self.mk(hir::ExprKind::ArrayLit(vec![]), ty, span);
        lets.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(init),
            },
            span,
        });
        l
    }

    /// `array.push(v)` for array local `array` of type `ty`.
    fn push_to(&mut self, array: hir::LocalId, ty: TyId, v: hir::Expr) -> hir::Stmt {
        let span = v.span;
        let target = self.mk(hir::ExprKind::Local(array, UseMode::BorrowMut), ty, span);
        let unit = self.cx.ty.unit;
        let call = self.intrinsic(hir::Intrinsic::ArrayPush, vec![target, v], unit, span);
        hir::Stmt {
            kind: hir::StmtKind::Expr(call),
            span,
        }
    }

    /// The runtime function for component `c`.
    fn component_runtime_fn(
        &mut self,
        p: &Provider,
        c: &Component,
        tag: &str,
        span: Span,
    ) -> Option<(DefId, &'static str)> {
        if !c.is_async {
            return Some((p.component, "jsxComponent"));
        }
        if let Some(d) = p.async_component {
            return Some((d, "jsxAsyncComponent"));
        }
        self.cx.error(
            Diagnostic::error(
                format!(
                    "the JSX provider '{}' does not support async components",
                    p.source
                ),
                span,
            )
            .with_note(format!(
                "<{tag}> returns a promise; the provider would need to export `jsxAsyncComponent`"
            )),
        );
        None
    }

    /// The runtime's `component` parameter type for props `props` and a component of type
    /// `natural` (that type itself where the runtime's signature leaves it open).
    fn runtime_component_type(&mut self, d: DefId, props: TyId, natural: TyId) -> TyId {
        let at = self.cx.fn_info(d).name_span;
        let c = self.fn_callable(d, String::new(), at);
        let [component, props_param, ..] = &c.params[..] else {
            return natural;
        };
        let mut slots = vec![None; c.slot_names.len()];
        self.cx.match_ty(props_param.ty, props, &mut slots);
        self.cx.match_ty(component.ty, natural, &mut slots);
        if slots.iter().any(Option::is_none) {
            return natural;
        }
        self.cx.subst_known(component.ty, &slots)
    }

    /// Does the result of function type `natural` convert to the result of `expected`?
    fn result_fits(&mut self, natural: TyId, expected: TyId) -> bool {
        let (TyKind::FnPtr { ret: from, .. }, TyKind::FnPtr { ret: to, .. }) = (
            self.cx.ty.kind(natural).clone(),
            self.cx.ty.kind(expected).clone(),
        ) else {
            return true;
        };
        let probe = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), from, Span::DUMMY);
        self.try_coerce(probe, to).is_ok()
    }

    fn component(&mut self, name: &ast::JsxName, tag: &str) -> Option<Component> {
        let callee = callee_ast(name);
        if let Some((d, id_span)) = self.named_fn(&callee) {
            self.cx.rec_ref(id_span, Target::Def(d));
            return self.fn_component(d, callee, tag);
        }
        let value = self.expr(&callee, None, Want::Borrow);
        let TyKind::FnPtr { params, ret, .. } = self.cx.ty.kind(value.ty).clone() else {
            if !self.cx.ty.is_bottom(value.ty) {
                let shown = self.cx.display(value.ty);
                self.not_component(
                    tag,
                    name.span(),
                    format!("its type `{shown}` is not a function `(props: P) => JSX.Element`"),
                );
            }
            return None;
        };
        let [props] = params[..] else {
            let why = "a component takes one parameter, its props".to_string();
            self.not_component(tag, name.span(), why);
            return None;
        };
        Some(Component {
            func: ComponentFn::Value(value),
            props,
            ret,
            slot_names: vec![],
            is_async: self.cx.ty.promise_payload(ret).is_some(),
            identity: format!("{}#{tag}", self.cx.modules[self.module].path),
        })
    }

    /// The named (non-extern) function `callee` denotes, with the span naming it.
    fn named_fn(&mut self, callee: &ast::Expr) -> Option<(DefId, Span)> {
        let id = match &callee.kind {
            ast::ExprKind::Ident(id) => id.clone(),
            _ => match self.without_namespace(callee)?.kind {
                ast::ExprKind::Ident(id) => id,
                _ => return None,
            },
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d) if self.cx.try_fn(d).is_some_and(|f| f.kind != FnKind::Extern) => {
                Some((d, id.span))
            }
            _ => None,
        }
    }

    fn fn_component(&mut self, def: DefId, callee: ast::Expr, tag: &str) -> Option<Component> {
        let c = self.fn_callable(def, String::new(), callee.span);
        let f = self.cx.fn_info(def);
        let simple = f.name.rsplit("::").next().unwrap_or(&f.name).to_string();
        let identity = format!("{}#{simple}", self.cx.modules[f.module].path);
        if c.params.len() > 1 {
            let why = "a component takes one parameter, its props".to_string();
            self.not_component(tag, callee.span, why);
            return None;
        }
        let props = match c.params.first() {
            Some(p) => p.ty,
            None => self.cx.anon_type(&[], self.module),
        };
        Some(Component {
            func: ComponentFn::Named {
                def,
                callee,
                takes_props: c.params.len() == 1,
            },
            props,
            ret: c.ret,
            slot_names: c.slot_names,
            is_async: self.cx.ty.promise_payload(c.ret).is_some(),
            identity,
        })
    }

    /// TS2786: `'X' cannot be used as a JSX component.`
    pub(super) fn not_component(&mut self, tag: &str, span: Span, why: String) {
        self.cx.error(
            Diagnostic::error(format!("'{tag}' cannot be used as a JSX component."), span)
                .with_note(why),
        );
    }
}
/// The expression a component tag names: `Card` or `ui.Card`.
fn callee_ast(name: &ast::JsxName) -> ast::Expr {
    match name {
        ast::JsxName::Ident(id) => synthetic(ast::ExprKind::Ident(id.clone()), id.span),
        ast::JsxName::Member(parts) => {
            let Some((first, rest)) = parts.split_first() else {
                return synthetic(ast::ExprKind::Lit(ast::Lit::Null), Span::DUMMY);
            };
            let mut e = synthetic(ast::ExprKind::Ident(first.clone()), first.span);
            for part in rest {
                let span = e.span.to(part.span);
                let kind = ast::ExprKind::Member {
                    object: Box::new(e),
                    prop: part.clone(),
                    optional: false,
                };
                e = synthetic(kind, span);
            }
            e
        }
        ast::JsxName::Namespaced(ns, id) => {
            synthetic(ast::ExprKind::Ident(id.clone()), ns.span.to(id.span))
        }
    }
}

/// A compiler-built expression (no node id of its own) at the source `span` it stands for.
pub(super) fn synthetic(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}
