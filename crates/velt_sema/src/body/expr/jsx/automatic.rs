//! TypeScript's automatic JSX runtime (`"jsx": "react-jsx"`; docs/internals/contracts/jsx.md
//! "Automatic runtimes"): a runtime that exports `jsx(type, props, key)` and no `jsxComponent`,
//! as React's and sigx's do. Every element is one call of its `jsx`, as `tsc` emits it:
//!
//! - `<box a={x}>c</box>` → `jsx("box", { a: x, children: c }, key)`;
//! - `<Card a={x} />` → `jsx(Card, { a: x }, key)`, the component passed as it is (a function
//!   with properties keeps them: sigx tells its components by `__setup`);
//! - `<>c</>` → `jsx(Fragment, { children: c }, key)`, with the runtime's `Fragment` value.
//!
//! The attributes are checked as `tsc` checks them: against `JSX.IntrinsicElements[tag]` or
//! the component's props (missing and unknown props, contextual types of arrow functions,
//! children). The props object is then the `Record<string, V>` the runtime's `props` parameter
//! takes (an index signature, `{ [key: string]: any }`, is one), holding the attributes written,
//! in source order: a new object per element, as in TypeScript. One child is the child itself,
//! several are an array, none leaves `children` out. The `JSX` types are the module's `JSX.X`:
//! the runtime's exports, else the global namespace `JSX` (`declare global`, as sigx declares
//! them; `crate::namespaces`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::super::record_call::RecordKey;
use super::children::real_children;
use super::component::ComponentFn;
use super::key::is_key;
use super::props::ElementProps;
use super::provider::{children_field_of, function, string_const, Provider};
use crate::body::{FnCx, LocalKind, Want};
use crate::collect::export_of;
use crate::ctx::{Ctx, Item};
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId, TyKind, UseMode};
use crate::resolve::TyEnv;

/// What an automatic runtime's `jsx` is called with.
pub(crate) struct Automatic {
    /// The runtime's `Fragment`: the `type` of a fragment (a value, sigx's `Symbol.for(…)`).
    pub fragment: DefId,
    /// `Record<string, V>`: the props object built for `jsx`'s `props` parameter.
    pub record: DefId,
    pub value: TyId,
    /// `jsx` takes a key (a third parameter).
    pub takes_key: bool,
}

/// The namespace the runtime's types are named under.
const JSX: &str = "JSX";

/// An automatic runtime of module `m` (the runtime module `t`, its `jsx` function `jsx`).
pub(super) fn load(
    cx: &mut Ctx,
    m: usize,
    t: usize,
    source: String,
    jsx: DefId,
    at: Span,
) -> Option<Provider> {
    let params: Vec<TyId> = cx.fn_info(jsx).params.iter().map(|p| p.ty).collect();
    if !(2..=3).contains(&params.len()) {
        let n = params.len();
        cx.error(
            Diagnostic::error(
                format!("`jsx` of the JSX provider '{source}' must take 2 or 3 parameters (type, props, key), not {n}"),
                at,
            )
            .with_note(NOTE),
        );
        return None;
    }
    let fragment = match export_of(cx, t, "Fragment") {
        Some(Item::Def(d)) if cx.global(d).is_some() => d,
        _ => {
            cx.error(
                Diagnostic::error(
                    format!("the JSX provider '{source}' does not export `Fragment`"),
                    at,
                )
                .with_note("an automatic runtime (`jsx(type, props, key)`) exports the value a fragment's `type` is, e.g. `export const Fragment = Symbol.for(\"Fragment\")`"),
            );
            return None;
        }
    };
    let (record, value) = props_record(cx, params[1], &source, at)?;
    let Some(intrinsics) = namespace_type(cx, m, "IntrinsicElements", at) else {
        cx.error(
            Diagnostic::error(
                format!("the JSX provider '{source}' declares no `JSX.IntrinsicElements`"),
                at,
            )
            .with_note("declare it with the runtime, or in `declare global { namespace JSX { interface IntrinsicElements { … } } }`, as in TypeScript"),
        );
        return None;
    };
    let ret = cx.fn_info(jsx).ret;
    let element = namespace_type(cx, m, "Element", at).unwrap_or(ret);
    let children_field = match namespace_type(cx, m, "ElementChildrenAttribute", at) {
        Some(ty) => children_field_of(cx, ty, &source, at)?,
        None => "children".to_string(),
    };
    let void_elements = string_const(
        cx,
        t,
        &source,
        at,
        "jsxVoidElements",
        "br hr img",
        "the compiler reads the tags at compile time",
    )?
    .map(|list| list.split_whitespace().map(str::to_string).collect());
    Some(Provider {
        source,
        jsx,
        fragment: jsx,
        component: jsx,
        async_component: function(cx, t, "jsxAsyncComponent").or(Some(jsx)),
        precompile: None,
        text_separator: None,
        sole_empty: None,
        void_elements,
        component_attrs: None,
        element,
        child: value,
        attr_value: value,
        intrinsics,
        children_field,
        automatic: Some(Automatic {
            fragment,
            record,
            value,
            takes_key: params.len() == 3,
        }),
    })
}

const NOTE: &str = "an automatic runtime is TypeScript's `react-jsx` one: `jsx(type, props, key)` (docs/internals/contracts/jsx.md \"Automatic runtimes\")";

/// The record `jsx`'s `props` parameter `param` takes: `Record<string, V>`, also as an index
/// signature with named members and with `null` or `undefined`; `any` takes a
/// `Record<string, any>`.
fn props_record(cx: &mut Ctx, param: TyId, source: &str, at: Span) -> Option<(DefId, TyId)> {
    let record = cx.prelude_adt("Record")?;
    let core = cx.ty.core(param).unwrap_or(param);
    if cx.dynamic(core).is_some() {
        return Some((record, core));
    }
    let base = cx.record_view(core).map_or(core, |(base, _)| base);
    if let TyKind::Adt(d, args) = cx.ty.kind(base) {
        if *d == record && args.len() == 2 && args[0] == cx.ty.str_ {
            return Some((record, args[1]));
        }
    }
    let shown = cx.display(param);
    cx.error(
        Diagnostic::error(
            format!("the `props` parameter of `jsx` of the JSX provider '{source}' has type `{shown}`, which Velt cannot pass element props as yet"),
            at,
        )
        .with_note("an automatic runtime takes its props as `Record<string, V>`, an index signature (`{ [key: string]: any }`) or `any`"),
    );
    None
}

/// The type `JSX.<name>` names in module `m`, if it names one.
fn namespace_type(cx: &mut Ctx, m: usize, name: &str, at: Span) -> Option<TyId> {
    let path: Vec<ast::Ident> = [JSX, name]
        .iter()
        .map(|n| ast::Ident {
            name: n.to_string(),
            span: at,
        })
        .collect();
    let item = cx.lookup_path_at(m, &path, at)?;
    let written = ast::TypeExpr {
        kind: ast::TypeExprKind::Named { path, args: vec![] },
        span: at,
    };
    Some(cx.item_type(item, name, &[], &written, &TyEnv::new(m, &[])))
}

impl FnCx<'_, '_> {
    /// Element or fragment `el` through automatic runtime `p`.
    pub(super) fn jsx_automatic(
        &mut self,
        p: &Provider,
        a: &Automatic,
        el: &ast::JsxElement,
    ) -> hir::Expr {
        let mut lets = vec![];
        let parts = match &el.name {
            None => self.auto_fragment(p, a, el),
            Some(name) => match super::intrinsic_tag(name) {
                Some(tag) => self.auto_intrinsic(p, el, &tag, name.span(), &mut lets),
                None => self.auto_component(p, el, name, &mut lets),
            },
        };
        let Some((ty, props, key)) = parts else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        let mut args = vec![ty, props];
        if a.takes_key {
            args.push(key);
        } else if let Some(ast::JsxAttr::Named { span, .. }) = el.attrs.iter().find(|x| is_key(x)) {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "the JSX provider '{}' takes no `key`: its `jsx` has two parameters",
                        p.source
                    ),
                    *span,
                )
                .with_note(NOTE),
            );
        }
        let call = self.jsx_call(p, p.jsx, "jsx", args, el.span);
        self.with_lets(lets, call)
    }

    /// `jsx(Fragment, { children }, key)`'s arguments.
    fn auto_fragment(
        &mut self,
        p: &Provider,
        a: &Automatic,
        el: &ast::JsxElement,
    ) -> Option<(hir::Expr, hir::Expr, hir::Expr)> {
        let ty = self.global_read(a.fragment, Want::Borrow, el.span);
        let mut entries = vec![];
        if let Some(c) = self.auto_children(p, &el.children, el.span) {
            entries.push((p.children_field.clone(), c));
        }
        let key = self.jsx_key(p, el);
        let props = self.props_record(a, entries, el.span);
        Some((ty, props, key))
    }

    /// `jsx("tag", { …attributes, children }, key)`'s arguments.
    fn auto_intrinsic(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        tag: &str,
        tag_span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<(hir::Expr, hir::Expr, hir::Expr)> {
        self.void_children(p, el, tag);
        let attrs = self.intrinsic_attrs(p, el, tag, tag_span, lets)?;
        let mut entries: Vec<(String, hir::Expr)> =
            attrs.into_iter().map(|x| (x.name, x.value)).collect();
        if let Some(c) = self.auto_children(p, &el.children, el.span) {
            entries.push((p.children_field.clone(), c));
        }
        let key = self.jsx_key(p, el);
        let a = p.automatic.as_ref().expect("ICE: an automatic runtime");
        let props = self.props_record(a, entries, el.span);
        Some((self.str_lit(tag, tag_span), props, key))
    }

    /// `jsx(Component, { …props }, key)`'s arguments.
    fn auto_component(
        &mut self,
        p: &Provider,
        el: &ast::JsxElement,
        name: &ast::JsxName,
        lets: &mut Vec<hir::Stmt>,
    ) -> Option<(hir::Expr, hir::Expr, hir::Expr)> {
        let tag = name.to_source();
        let c = self.component(name, &tag)?;
        let ElementProps {
            props,
            type_args,
            key,
            ..
        } = self.component_props(p, el, &c, &tag, lets)?;
        if type_args.contains(&self.cx.ty.error) {
            return None;
        }
        let ty = match c.func {
            ComponentFn::Named { callee, .. } => self.expr(&callee, None, Want::Borrow),
            ComponentFn::Value(v) => v,
        };
        Some((ty, props, key))
    }

    /// The `children` of an element: none, the only child, or an array of all of them, each as
    /// a `JSX.Child`.
    fn auto_children(
        &mut self,
        p: &Provider,
        children: &[ast::JsxChild],
        span: Span,
    ) -> Option<hir::Expr> {
        let kids = real_children(children);
        let mut hs: Vec<hir::Expr> = kids
            .iter()
            .map(|c| {
                let h = self.child_value(p, c, p.child);
                self.as_child(p, h)
            })
            .collect();
        match hs.len() {
            0 => None,
            1 => hs.pop(),
            _ => {
                let ty = self.cx.ty.array(p.child);
                let all = self.mk(H::ArrayLit(hs), ty, span);
                Some(self.as_child(p, all))
            }
        }
    }

    /// A new `Record<string, V>` holding `entries` in order, each converted to `V`.
    pub(super) fn props_record(
        &mut self,
        a: &Automatic,
        entries: Vec<(String, hir::Expr)>,
        span: Span,
    ) -> hir::Expr {
        let k = self.cx.ty.str_;
        let ty = self.cx.ty.intern(TyKind::Adt(a.record, vec![k, a.value]));
        let new = self.mk(
            H::New {
                def: a.record,
                type_args: vec![k, a.value],
                args: vec![],
            },
            ty,
            span,
        );
        let t = self.new_local("<props>", ty, false, span, LocalKind::Temp);
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local: t,
                init: Some(new),
            },
            span,
        }];
        let what = "in the props the JSX runtime's `jsx` takes";
        for (name, h) in entries {
            let at = h.span;
            let value = self.jsx_coerce(h, a.value, what);
            let recv = self.mk(H::Local(t, UseMode::Borrow), ty, span);
            let key = ast::Ident { name, span: at };
            let set = self.record_set_checked(recv, RecordKey::Name(&key), value, at);
            stmts.push(hir::Stmt {
                kind: S::Expr(set),
                span: at,
            });
        }
        let value = self.mk(H::Local(t, UseMode::Move), ty, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}
