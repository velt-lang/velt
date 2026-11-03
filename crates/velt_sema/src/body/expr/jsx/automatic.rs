//! TypeScript's automatic JSX runtime (`"jsx": "react-jsx"`; docs/internals/contracts/jsx.md
//! "Automatic runtimes"): a runtime that exports `jsx` or `jsxs`, chosen by
//! `provider::lowering_of`. Every element is one call, as `tsc` emits it:
//!
//! - `<box a={x}>c</box>` → `jsx("box", { a: x, children: c }, key)`;
//! - `<Card a={x} />` → `jsx(Card, { a: x }, key)`, the component passed as it is (a function
//!   with properties keeps them);
//! - `<>c</>` → `jsx(Fragment, { children: c }, key)`;
//! - with several children, or one spread child, `jsxs` instead of `jsx`.
//!
//! The props object is built in one function (`automatic_props`). The `JSX` types are looked up
//! as `tsc` does: the runtime's exported namespace `JSX`, then its flat exports (which `tsc` does
//! not read, and Velt's own runtimes use), then the global namespace `JSX`, where several modules
//! may each add to `IntrinsicElements`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::children::real_children;
use super::component::ComponentFn;
use super::key::is_key;
use super::props::is_constant;
use super::provider::{children_field_of, string_const, Provider};
use crate::body::places::is_place;
use crate::body::{FnCx, Want};
use crate::collect::export_of;
use crate::ctx::{Ctx, Item};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, DefId, ExprKind as H, LitValue, StmtKind as S, TyId, TyKind};
use crate::resolve::TyEnv;

/// What a factory is: a function, or a constant holding one (`export const jsx = (…) => …`).
#[derive(Clone, Copy)]
enum FactoryFn {
    Def(DefId),
    Value(DefId),
}

/// `jsx` or `jsxs` of an automatic runtime.
pub(crate) struct Factory {
    name: &'static str,
    func: FactoryFn,
    /// The parameter types: `type`, `props`, and the `key` and others when there are any.
    params: Vec<TyId>,
    /// The index of its rest parameter (`jsx(type, props, ...rest)`).
    rest: Option<usize>,
    /// `Record<string, V>`: the props object built for its `props` parameter.
    pub record: DefId,
    pub value: TyId,
}

/// A runtime's `Fragment`: a value (`Symbol.for(…)`) or a function.
#[derive(Clone, Copy)]
enum FragmentType {
    Value(DefId),
    Function(DefId),
}

/// What an automatic runtime exports and declares.
pub(crate) struct Automatic {
    jsx: Option<Factory>,
    jsxs: Option<Factory>,
    fragment: Option<FragmentType>,
    /// Every declaration of `JSX.IntrinsicElements` in the program: they merge.
    pub intrinsics: Vec<TyId>,
    /// Every declaration of `JSX.IntrinsicAttributes`.
    pub attributes: Vec<TyId>,
    /// `JSX.Element`, which a component's result must be.
    element: Option<TyId>,
    /// `JSX.ElementType` is declared: it, not `JSX.Element`, says what a component may return.
    element_type: bool,
    /// `JSX.ElementChildrenAttribute` is declared: children are checked against its field.
    pub children_checked: bool,
}

/// The namespace the runtime's types are named under.
const JSX: &str = "JSX";

const NOTE: &str = "an automatic runtime is TypeScript's `react-jsx` one: `jsx(type, props, key)` and `jsxs` (docs/internals/contracts/jsx.md \"Automatic runtimes\")";

/// Exported `name` of module `t` as a function or a constant holding a function, with its
/// parameter and result types.
fn callable_export(
    cx: &mut Ctx,
    t: usize,
    name: &str,
) -> Option<(FactoryFn, Vec<TyId>, TyId, bool)> {
    let Item::Def(d) = export_of(cx, t, name)? else {
        return None;
    };
    if cx.try_fn(d).is_some() {
        let f = cx.fn_info(d);
        let params = f.params.iter().map(|p| p.ty).collect();
        let ret = f.ret;
        let rest = crate::body::defaults::has_rest(cx, d);
        return Some((FactoryFn::Def(d), params, ret, rest));
    }
    cx.global(d)?;
    crate::body::driver::ensure_global(cx, d);
    let ty = cx.global(d)?.ty;
    let fn_ty = cx.call_type(ty).unwrap_or(ty);
    match cx.ty.kind(fn_ty).clone() {
        TyKind::FnPtr {
            params, ret, rest, ..
        } => Some((FactoryFn::Value(d), params, ret, rest)),
        _ => None,
    }
}

/// Do module `t`'s exports make it an automatic runtime: `jsx` or `jsxs` needing at most `type`,
/// `props` and `key`? (The provider contract's `jsx` needs five arguments.)
pub(super) fn exports_automatic(cx: &mut Ctx, t: usize) -> bool {
    ["jsx", "jsxs"].iter().any(|name| {
        let Some((func, params, _, rest)) = callable_export(cx, t, name) else {
            return false;
        };
        let required = match func {
            FactoryFn::Def(d) => {
                let f = cx.fn_info(d);
                let required = f.params.iter().filter(|p| p.default.is_none());
                required.count() - usize::from(rest)
            }
            FactoryFn::Value(_) => {
                let fixed = params.len() - usize::from(rest);
                let required = params[..fixed].iter();
                required.filter(|p| !accepts_undefined(cx, **p)).count()
            }
        };
        required <= 3
    })
}

/// Reports an exported `jsx` that cannot be called, which neither form can use; true if it did.
pub(super) fn report_not_callable(cx: &mut Ctx, t: usize, source: &str, at: Span) -> bool {
    let Some(Item::Def(d)) = export_of(cx, t, "jsx") else {
        return false;
    };
    if callable_export(cx, t, "jsx").is_some() {
        return false;
    }
    let shown = cx.global(d).map(|g| g.ty).map(|ty| cx.display(ty));
    let what = shown.map_or(String::new(), |s| format!(" (it has type `{s}`)"));
    cx.error(
        Diagnostic::error(
            format!("`jsx` of the JSX provider '{source}' is not a function{what}"),
            at,
        )
        .with_note("a factory is called with what each element gives, so it needs a function type: `jsx(type, props, key)` for TypeScript's automatic runtime, or the provider contract's `jsx` with `jsxComponent` (docs/internals/contracts/jsx.md)"),
    );
    true
}

/// An automatic runtime for module `m` (the runtime is module `t`).
pub(super) fn load(cx: &mut Ctx, m: usize, t: usize, source: String, at: Span) -> Option<Provider> {
    let jsx = factory(cx, t, "jsx", &source, at)?;
    let jsxs = factory(cx, t, "jsxs", &source, at)?;
    let (func, value, ret) = match jsx.as_ref().or(jsxs.as_ref()) {
        Some((f, ret)) => (f.func, f.value, *ret),
        None => return None,
    };
    let fragment = match export_of(cx, t, "Fragment") {
        Some(Item::Def(d)) if cx.global(d).is_some() => Some(FragmentType::Value(d)),
        Some(Item::Def(d)) if cx.try_fn(d).is_some() => Some(FragmentType::Function(d)),
        _ => None,
    };
    let exported = exports_jsx_namespace(cx, t);
    let intrinsics = namespace_types(cx, m, t, exported, "IntrinsicElements", at);
    let attributes = namespace_types(cx, m, t, exported, "IntrinsicAttributes", at);
    let declared_element = namespace_types(cx, m, t, exported, "Element", at)
        .first()
        .copied();
    let element = declared_element.unwrap_or(ret);
    let element_type = !namespace_types(cx, m, t, exported, "ElementType", at).is_empty();
    let children_attr = namespace_types(cx, m, t, exported, "ElementChildrenAttribute", at);
    let children_field = match children_attr.first() {
        Some(ty) => children_field_of(cx, *ty, &source, at)?,
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
    // The contract's fields are not read in this mode.
    let def = match func {
        FactoryFn::Def(d) | FactoryFn::Value(d) => d,
    };
    Some(Provider {
        source,
        jsx: def,
        fragment: def,
        component: def,
        async_component: None,
        precompile: None,
        text_separator: None,
        sole_empty: None,
        void_elements,
        component_attrs: None,
        element,
        child: value,
        attr_value: value,
        intrinsics: cx.ty.error,
        children_field,
        automatic: Some(Automatic {
            jsx: jsx.map(|(f, _)| f),
            jsxs: jsxs.map(|(f, _)| f),
            fragment,
            intrinsics,
            attributes,
            element: declared_element,
            element_type,
            children_checked: !children_attr.is_empty(),
        }),
    })
}

/// Factory `name` of runtime module `t`, with its result type: `Some(None)` without one, `None`
/// once a shape the compiler cannot call was reported.
fn factory(
    cx: &mut Ctx,
    t: usize,
    name: &'static str,
    source: &str,
    at: Span,
) -> Option<Option<(Factory, TyId)>> {
    let Some((func, params, ret, rest)) = callable_export(cx, t, name) else {
        return Some(None);
    };
    let rest = rest.then(|| params.len() - 1);
    let extra_required = (params.iter().enumerate().skip(3))
        .any(|(i, p)| Some(i) != rest && !accepts_undefined(cx, *p));
    if params.len() < 2 || rest.is_some_and(|r| r < 2) || extra_required {
        cx.error(
            Diagnostic::error(
                format!("`{name}` of the JSX provider '{source}' must take `type` and `props`, then `key`, and any further parameters must be optional"),
                at,
            )
            .with_note(NOTE),
        );
        return None;
    }
    let (record, value) = props_record(cx, name, params[1], source, at)?;
    let f = Factory {
        name,
        func,
        params,
        rest,
        record,
        value,
    };
    Some(Some((f, ret)))
}

/// Can a parameter of type `t` be passed `undefined` when nothing is written for it?
fn accepts_undefined(cx: &Ctx, t: TyId) -> bool {
    cx.ty.nothing_of(t).is_some() || cx.dynamic(t).is_some()
}

/// The record the `props` parameter `param` of factory `name` takes: `Record<string, V>`, also as
/// an index signature with named members and with `null` or `undefined`. `any`, `unknown`,
/// `object` and a type parameter take a `Record<string, any>`.
fn props_record(
    cx: &mut Ctx,
    name: &str,
    param: TyId,
    source: &str,
    at: Span,
) -> Option<(DefId, TyId)> {
    let record = cx.prelude_adt("Record")?;
    let core = cx.ty.core(param).unwrap_or(param);
    if cx.dynamic(core).is_some() || matches!(cx.ty.kind(core), TyKind::Param(_)) {
        let any = cx.any?.any;
        return Some((record, cx.ty.intern(TyKind::Dyn(any, vec![]))));
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
            format!("the `props` parameter of `{name}` of the JSX provider '{source}' has type `{shown}`, which Velt cannot pass element props as"),
            at,
        )
        .with_note("the props hold whatever an element was given, and a Velt object has a fixed shape: take them as `Record<string, V>`, an index signature (`{ [key: string]: any }`), `any`, `object` or a type parameter"),
    );
    None
}

/// Does runtime module `t` export a namespace `JSX`? (It then is the only one `tsc` reads.)
fn exports_jsx_namespace(cx: &Ctx, t: usize) -> bool {
    JSX_MEMBERS
        .iter()
        .any(|m| export_of(cx, t, &format!("{JSX}.{m}")).is_some())
}

/// The members of `JSX` the compiler reads.
const JSX_MEMBERS: [&str; 5] = [
    "Element",
    "ElementType",
    "IntrinsicElements",
    "IntrinsicAttributes",
    "ElementChildrenAttribute",
];

/// The types `JSX.<name>` names for the JSX of module `m` with runtime module `t`, the one that
/// wins first. As in `tsc`, a namespace the runtime exports (`exported`) is the only one read;
/// else the runtime's flat exports, then the module's own and every other declaration of the
/// global namespace, which merge.
fn namespace_types(
    cx: &mut Ctx,
    m: usize,
    t: usize,
    exported: bool,
    name: &str,
    at: Span,
) -> Vec<TyId> {
    let key = format!("{JSX}.{name}");
    let mut items = vec![];
    if let Some(i) = export_of(cx, t, &key) {
        items.push((i, t));
    }
    if !exported {
        if let Some(i @ Item::Def(d)) = export_of(cx, t, name) {
            if cx.try_fn(d).is_none() && cx.global(d).is_none() {
                items.push((i, t));
            }
        }
        let declaring = cx.global_namespaces.get(JSX).cloned().unwrap_or_default();
        for dm in std::iter::once(m).chain(declaring) {
            if let Some(i) = cx.scopes[dm].items.get(&key).copied() {
                items.push((i, dm));
            }
        }
    }
    let names = [JSX, name];
    let path = names.iter().map(|n| ast::Ident {
        name: n.to_string(),
        span: at,
    });
    let written = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: path.collect(),
            args: vec![],
        },
        span: at,
    };
    let mut out = vec![];
    for (item, dm) in items {
        let ty = cx.item_type(item, name, &[], &written, &TyEnv::new(dm, &[]));
        if !out.contains(&ty) {
            out.push(ty);
        }
    }
    out
}

/// Does TypeScript call `jsxs` for these children? For two or more, or one spread child.
fn static_children(children: &[ast::JsxChild]) -> bool {
    let kids = real_children(children);
    kids.len() > 1 || matches!(kids.first(), Some(ast::JsxChild::Spread { .. }))
}

/// What an element's `type` argument comes from.
enum ElementType {
    Fragment,
    Tag(String, Span),
    Component(Box<ComponentFn>, Span),
}

impl FnCx<'_, '_> {
    /// Element or fragment `el` through automatic runtime `p`: `f(type, props, key)`, evaluated in
    /// that order; a key after a spread is evaluated in its place in the props.
    pub(super) fn jsx_automatic(
        &mut self,
        p: &Provider,
        a: &Automatic,
        el: &ast::JsxElement,
    ) -> hir::Expr {
        let target = self.element_target(p, a, el);
        let (Some(f), Some((ty, decl, slots, tag))) = (self.element_factory(p, a, el), target)
        else {
            self.jsx_loose(p, el);
            return self.error_expr(el.span);
        };
        let declared_key = match ty {
            ElementType::Component(..) => self.declared_key(a),
            _ => None,
        };
        let (key, key_at) = self.auto_key(p, f, el, declared_key);
        // Evaluated with the props: a key after a spread, and one the factory does not take.
        let mut key_stmts = vec![];
        let takes_key = f.params.len() > 2;
        let (key, in_props) = match (key, key_at, takes_key) {
            (Some(k), Some(at), true) => {
                let read = self.moved_temp(k, &mut key_stmts);
                (Some(read), Some(at))
            }
            (Some(k), at, false) => {
                let span = k.span;
                key_stmts.push(hir::Stmt {
                    kind: S::Expr(k),
                    span,
                });
                (None, Some(at.unwrap_or(el.attrs.len() + 1)))
            }
            (k, _, _) => (k, None),
        };
        let staged = in_props.map(|at| (at, key_stmts));
        // A key written after a spread is the key, whatever the spread held.
        let drop_key = key_at.is_some();
        let props = self.element_props(p, f, el, decl, &slots, &tag, staged, drop_key);
        if props.type_args.contains(&self.cx.ty.error) {
            return self.error_expr(el.span);
        }
        let ty = match ty {
            ElementType::Fragment => self.fragment_type(a, el.span),
            ElementType::Tag(tag, span) => self.str_lit(&tag, span),
            ElementType::Component(func, span) => {
                self.component_type(*func, &props.type_args, &props.inferred, span)
            }
        };
        // With a key among the props, they are bound before the call, after the `type`.
        let (mut lets, args_props) = match in_props.is_some() && takes_key {
            true => (props.stmts, props.read),
            false => (vec![], self.with_lets(props.stmts, props.read)),
        };
        let ty = match lets.is_empty() || is_constant(&ty) || is_place(&ty) {
            true => ty,
            false => {
                let mut first = vec![];
                let read = self.moved_temp(ty, &mut first);
                first.append(&mut lets);
                lets = first;
                read
            }
        };
        let mut args = vec![ty, args_props];
        match f.rest == Some(2) {
            // `jsx(type, props, ...rest)`: a written key is the first of the rest.
            true => {
                let ty = f.params[2];
                args.push(self.mk(H::ArrayLit(key.into_iter().collect()), ty, el.span));
            }
            false => args.extend(key),
        }
        for (i, t) in f.params.iter().copied().enumerate().skip(args.len()) {
            let u = match f.rest == Some(i) {
                true => self.mk(H::ArrayLit(vec![]), t, el.span),
                false => self.undefined_arg(p, t, el.span),
            };
            args.push(u);
        }
        let call = self.factory_call(p, f, args, el.span);
        self.with_lets(lets, call)
    }

    /// The factory TypeScript calls for `el`; reported when the runtime does not export it.
    fn element_factory<'a>(
        &mut self,
        p: &Provider,
        a: &'a Automatic,
        el: &ast::JsxElement,
    ) -> Option<&'a Factory> {
        let (name, f) = match static_children(&el.children) {
            true => ("jsxs", &a.jsxs),
            false => ("jsx", &a.jsx),
        };
        if f.is_none() {
            self.cx.error(
                Diagnostic::error(
                    format!("the JSX provider '{}' does not export `{name}`", p.source),
                    el.span,
                )
                .with_note("TypeScript's automatic transform calls `jsxs` for an element with several children or a spread child, and `jsx` for the others"),
            );
        }
        f.as_ref()
    }

    /// What `el` is: its type, its declared attributes, its type parameters, its name in messages.
    #[allow(clippy::type_complexity)]
    fn element_target(
        &mut self,
        p: &Provider,
        a: &Automatic,
        el: &ast::JsxElement,
    ) -> Option<(
        ElementType,
        super::automatic_decl::Declared,
        Vec<String>,
        String,
    )> {
        let Some(name) = &el.name else {
            if a.fragment.is_none() {
                self.cx.error(
                    Diagnostic::error(
                        format!("the JSX provider '{}' does not export `Fragment`", p.source),
                        el.span,
                    )
                    .with_note("a fragment's `type` is the runtime's `Fragment`, a value (`Symbol.for(\"Fragment\")`) or a function"),
                );
                return None;
            }
            let decl = self.open_props();
            return Some((ElementType::Fragment, decl, vec![], "<>".to_string()));
        };
        if let Some(tag) = super::intrinsic_tag(name) {
            self.void_children(p, el, &tag);
            let decl = self.tag_props(a, &tag, name.span())?;
            let ty = ElementType::Tag(tag.clone(), name.span());
            return Some((ty, decl, vec![], tag));
        }
        let tag = name.to_source();
        let c = self.component(name, &tag, true)?;
        // A tag holding a string (polymorphic `as`) is that intrinsic element; a known one is
        // checked as one.
        if let ComponentFn::Value(v) = &c.func {
            if let Some(LitValue::Str(s)) = self.cx.lit_value(v.ty) {
                let decl = self.tag_props(a, &s, name.span())?;
                let ty = ElementType::Component(Box::new(c.func), name.span());
                return Some((ty, decl, vec![], tag));
            }
        }
        if !self.component_result_fits(a, c.ret, &tag, name.span()) {
            return None;
        }
        let decl = self.component_props_decl(a, c.props, &c.slot_names, &tag, name.span())?;
        let ty = ElementType::Component(Box::new(c.func), name.span());
        Some((ty, decl, c.slot_names, tag))
    }

    /// TS2786: a component's result must be a `JSX.Element` or `null`, unless `JSX.ElementType`
    /// says otherwise. Without `JSX.Element`, `tsc` has nothing to check it against either.
    fn component_result_fits(&mut self, a: &Automatic, ret: TyId, tag: &str, span: Span) -> bool {
        let Some(element) = a.element.filter(|_| !a.element_type) else {
            return true;
        };
        let core = self.cx.ty.core(ret).unwrap_or(ret);
        let open = self.cx.ty.has_error(core)
            || self.cx.ty.is_bottom(core)
            || self.cx.dynamic(core).is_some()
            || self.cx.has_type_params(core);
        if open || self.cx.ty.nothing_of(ret).is_some() && core == ret {
            return true;
        }
        let probe = self.mk(H::Lit(hir::Lit::Unit), core, span);
        if self.try_coerce(probe, element).is_ok() {
            return true;
        }
        let (r, e) = (self.cx.display(ret), self.cx.display(element));
        let why =
            format!("its return type `{r}` is not a valid JSX element: not assignable to `{e}`");
        self.not_component(tag, span, why);
        false
    }

    /// The runtime's `Fragment` as a value.
    fn fragment_type(&mut self, a: &Automatic, span: Span) -> hir::Expr {
        match a.fragment {
            Some(FragmentType::Value(d)) => self.global_read(d, Want::Borrow, span),
            Some(FragmentType::Function(d)) => {
                // Nothing gives a generic one type arguments: they are `unknown`, as in `tsc`.
                let n = self
                    .fn_callable(d, "`Fragment`".to_string(), span)
                    .slot_names
                    .len();
                let unknown = self.cx.any.map(|defs| TyKind::Dyn(defs.unknown, vec![]));
                let unknown = unknown.map(|k| self.cx.ty.intern(k));
                self.fn_ref_slots(d, "Fragment", vec![unknown; n], None, span)
            }
            None => self.error_expr(span),
        }
    }

    /// The component as the `type` argument: as itself, a generic one at its type arguments.
    fn component_type(
        &mut self,
        func: ComponentFn,
        type_args: &[TyId],
        inferred: &[Option<TyId>],
        span: Span,
    ) -> hir::Expr {
        match func {
            ComponentFn::Named { callee, .. } => match self.named_fn(&callee) {
                Some((d, _)) if !type_args.is_empty() => {
                    let slots: Vec<Option<TyId>> = type_args.iter().copied().map(Some).collect();
                    let name = self.cx.fn_info(d).name.clone();
                    // The inferred arguments must meet the constraints, as in a call.
                    let c = self.fn_callable(d, format!("`{name}`"), span);
                    let before = self.cx.diags.len();
                    self.solve_slots(&c, inferred, true, span, &[]);
                    if self.cx.diags.len() > before {
                        return self.error_expr(span);
                    }
                    self.fn_ref_slots(d, &name, slots, None, span)
                }
                _ => self.expr(&callee, None, Want::Borrow),
            },
            ComponentFn::Value(v) => {
                // The runtime keeps the component: a variable used again passes a share.
                if is_place(&v) {
                    self.f.soft_moves.push(v.span);
                }
                v
            }
        }
    }

    /// `f(args)` for factory `f`, each argument converted to its parameter.
    fn factory_call(
        &mut self,
        p: &Provider,
        f: &Factory,
        args: Vec<hir::Expr>,
        span: Span,
    ) -> hir::Expr {
        let (name, source) = (f.name, p.source.clone());
        let mut report = |s: &mut Self, param: &str, expected: TyId, found: &hir::Expr| {
            let (e, g) = (s.cx.display(expected), s.cx.display(found.ty));
            s.cx.error(
                Diagnostic::error(
                    format!("`{name}` of the JSX provider '{source}' does not accept this: parameter `{param}` has type `{e}`, found `{g}`"),
                    found.span,
                )
                .with_note(NOTE),
            );
        };
        let d = match f.func {
            FactoryFn::Def(d) => return self.call_checked(d, args, span, &mut report),
            FactoryFn::Value(d) => d,
        };
        let mut callee = self.global_read(d, Want::Borrow, span);
        if self.cx.is_callable_object(callee.ty) {
            let Some(f) = self.call_of_callable(callee, span) else {
                return self.error_expr(span);
            };
            callee = f;
        }
        let TyKind::FnPtr { ret, .. } = self.cx.ty.kind(callee.ty).clone() else {
            return self.error_expr(span);
        };
        let mut hargs = vec![];
        for (i, (h, t)) in args.into_iter().zip(&f.params).enumerate() {
            hargs.push(match self.try_coerce(h, *t) {
                Ok(h) => h,
                Err(h) => {
                    report(
                        self,
                        ["type", "props", "key"].get(i).unwrap_or(&"?"),
                        *t,
                        &h,
                    );
                    h
                }
            });
        }
        let site = self.site_of_value(&callee);
        self.throw_src(ThrowSrc::Indirect(callee.ty, site, span));
        let kind = H::Call {
            callee: Callee::Indirect(Box::new(callee)),
            args: hargs,
        };
        self.mk(kind, ret, span)
    }

    /// `undefined` for a parameter of type `t` that nothing is written for.
    fn undefined_arg(&mut self, p: &Provider, t: TyId, span: Span) -> hir::Expr {
        if self.cx.ty.nothing_of(t).is_some() {
            return self.mk(H::Lit(hir::Lit::Null), t, span);
        }
        if self.cx.dynamic(t).is_some() {
            let e = super::component::synthetic(ast::ExprKind::Lit(ast::Lit::Undefined), span);
            let h = self.expr(&e, Some(t), Want::Move);
            return self.jsx_coerce(h, t, "as an argument of the JSX runtime's factory");
        }
        let shown = self.cx.display(t);
        self.cx.err(
            format!(
                "this element needs a `key`: the JSX provider '{}' takes it as `{shown}`",
                p.source
            ),
            span,
        );
        self.error_expr(span)
    }

    /// The `key` argument of factory `f` for `el` (`None` when it takes none and none is written)
    /// and, after a spread, the attribute index where it is evaluated. The attribute's value is
    /// passed as written, else `undefined`, as `tsc` does. A number going to a parameter that
    /// takes only a string is its decimal string. `declared` is the `key` field of
    /// `JSX.IntrinsicAttributes`, which a written value must fit.
    fn auto_key(
        &mut self,
        p: &Provider,
        f: &Factory,
        el: &ast::JsxElement,
        declared: Option<TyId>,
    ) -> (Option<hir::Expr>, Option<usize>) {
        let at = el.attrs.iter().position(is_key);
        if let Some(ast::JsxAttr::Named { span, .. }) = el.attrs.iter().filter(|a| is_key(a)).nth(1)
        {
            self.cx.err(
                "JSX elements cannot have multiple attributes with the same name.",
                *span,
            );
        }
        let in_rest = f.rest == Some(2);
        let kty = match f.params.get(2).copied() {
            Some(t) if in_rest => self.cx.ty.array_elem(t),
            k => k,
        };
        let Some(ast::JsxAttr::Named { value, span, .. }) = at.map(|i| &el.attrs[i]) else {
            let none = kty.filter(|_| !in_rest);
            return (none.map(|t| self.undefined_arg(p, t, el.span)), None);
        };
        let h = match value {
            Some(ast::JsxAttrValue::Str { value, span }) => self.str_lit(value, *span),
            Some(ast::JsxAttrValue::Expr { expr, .. }) => {
                // A `null` has no type of its own; any other value goes by what it is. A
                // parameter with only one nothing gets that one.
                let nothing = matches!(
                    expr.kind,
                    ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
                );
                match kty {
                    Some(t) if nothing && self.cx.ty.nothing_of(t).is_some() => {
                        return (Some(self.mk(H::Lit(hir::Lit::Null), t, expr.span)), None);
                    }
                    Some(t) if nothing => self.expr(expr, Some(t), Want::Move),
                    None if nothing => {
                        let any = self.any_ty();
                        self.expr(expr, any, Want::Move)
                    }
                    _ => self.expr(expr, None, Want::Move),
                }
            }
            Some(ast::JsxAttrValue::Element(inner)) => self.jsx_element(p, inner),
            None => self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, *span),
        };
        if let Some(d) = declared {
            let probe = self.mk(H::Lit(hir::Lit::Unit), h.ty, h.span);
            if self.try_coerce(probe, d).is_err() {
                self.not_assignable(d, &h, "in attribute 'key'");
                return (Some(self.error_expr(h.span)), None);
            }
        }
        let value = match kty {
            Some(t) => self.key_as(h, t, &p.source),
            None => h,
        };
        let after_spread = at.is_some_and(|k| {
            let before = &el.attrs[..k];
            before
                .iter()
                .any(|a| matches!(a, ast::JsxAttr::Spread { .. }))
        });
        (Some(value), at.filter(|_| after_spread))
    }

    /// The written key value `h` as parameter type `t`.
    fn key_as(&mut self, h: hir::Expr, t: TyId, source: &str) -> hir::Expr {
        let what = format!("as the `key` the JSX provider '{source}' takes");
        let probe = self.mk(H::Lit(hir::Lit::Unit), h.ty, h.span);
        if self.try_coerce(probe, t).is_err() && self.cx.ty.is_numeric(h.ty) {
            let (str_, span) = (self.cx.ty.str_, h.span);
            let s = self.intrinsic(hir::Intrinsic::ToString, vec![h], str_, span);
            return self.jsx_coerce(s, t, &what);
        }
        self.jsx_coerce(h, t, &what)
    }
}
