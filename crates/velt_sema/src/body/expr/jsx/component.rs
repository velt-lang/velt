//! Component elements (`<Card …>`, `<ui.Card …>`): the tag names a function
//! `(props: P) => …` that is passed **uncalled** with its props, exactly like TypeScript's
//! `jsx(Card, props)`: `jsxComponent(component, props, key, name)` (or `jsxAsyncComponent` when
//! it returns a promise). The runtime decides when the component runs (parent-first rendering,
//! context). The component's type is checked against the runtime's parameter type, so a runtime
//! may accept components returning more than `JSX.Element`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::attrs::attr_name;
use super::provider::Provider;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::FnKind;
use crate::hir::{self, DefId, TyId, TyKind};
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
    /// The props (with already built child elements) are evaluated first; the component runs
    /// when the runtime calls it.
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
        let Some((props, type_args)) = self.component_props(p, el, &c, &tag, &mut lets) else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        if type_args.contains(&self.cx.ty.error) {
            return self.error_expr(el.span);
        }
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
        let is_async = c.is_async;
        let Some(value) = self.component_value(c.func, expected, props.ty, el, &tag) else {
            return self.error_expr(el.span);
        };
        let key = self.jsx_key(p, el);
        let identity = self.str_lit(&c.identity, name.span());
        let mut args = vec![value, props, key, identity];
        let (d, f) = match self.component_directives(p, el, is_async, &tag) {
            None => (d, f),
            Some(None) => return self.error_expr(el.span),
            Some(Some((names, values, call))) => {
                args.push(names);
                args.push(values);
                (call, "jsxComponentDirectives")
            }
        };
        let call = self.jsx_call(p, d, f, args, el.span);
        self.with_lets(lets, call)
    }

    /// The directives of component element `el` (`client:load`, `client:media="…"`) as the
    /// arrays `names` and `values` (each converted to `JSX.AttrValue`, a bare one `true`), with
    /// `jsxComponentDirectives`: `None` when it has none (the call stays `jsxComponent`),
    /// `Some(None)` after an error.
    #[allow(clippy::type_complexity)]
    fn component_directives(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        is_async: bool,
        tag: &str,
    ) -> Option<Option<(hir::Expr, hir::Expr, DefId)>> {
        let dir = p.directives.as_ref()?;
        let written: Vec<&ast::JsxAttr> = el.attrs.iter().filter(|a| p.is_directive(a)).collect();
        let first = written.first()?;
        if is_async {
            let span = match first {
                ast::JsxAttr::Named { span, .. } | ast::JsxAttr::Spread { span, .. } => *span,
            };
            self.cx.error(
                Diagnostic::error(
                    format!("directives are not supported on async components (<{tag}>)"),
                    span,
                )
                .with_note(format!(
                    "the JSX provider '{}' receives directives through `jsxComponentDirectives`, which takes a synchronous component",
                    p.source
                )),
            );
            return Some(None);
        }
        let mut names = vec![];
        let mut values = vec![];
        for a in written {
            let ast::JsxAttr::Named { name, value, span } = a else {
                continue;
            };
            let (n, name_span) = attr_name(name);
            if names.iter().any(
                |h: &hir::Expr| matches!(&h.kind, hir::ExprKind::Lit(hir::Lit::Str(s)) if *s == n),
            ) {
                self.cx.err(
                    "JSX elements cannot have multiple attributes with the same name.",
                    name_span,
                );
            }
            let (h, _) = self.attr_value(p, value, Some(p.attr_value), *span);
            let note = format!(
                "directive values are passed to the JSX provider '{}' as `JSX.AttrValue`",
                p.source
            );
            values.push(self.jsx_coerce(h, p.attr_value, &note));
            names.push(self.str_lit(&n, name_span));
        }
        let str_ = self.cx.ty.str_;
        let names_ty = self.cx.ty.array(str_);
        let names = self.mk(hir::ExprKind::ArrayLit(names), names_ty, el.span);
        let values_ty = self.cx.ty.array(p.attr_value);
        let values = self.mk(hir::ExprKind::ArrayLit(values), values_ty, el.span);
        Some(Some((names, values, dir.call)))
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
