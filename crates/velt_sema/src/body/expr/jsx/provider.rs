//! The JSX runtime of a module (docs/internals/contracts/jsx.md "Required exports"): its factory
//! functions and `JSX` types, looked up once per module (`Ctx::jsx_providers`). A runtime that
//! lacks a required export is reported once, at the first JSX of the module; its JSX is then
//! skipped.

use std::rc::Rc;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::collect::export_of;
use crate::ctx::{Ctx, Item};
use crate::hir::{AdtKind, DefId, ExprKind as H, Lit, LitValue, TyId, TyKind};
use crate::resolve::TyEnv;

/// The SSR precompile exports (docs/internals/contracts/jsx.md "SSR precompile"); a runtime has all
/// of them or the precompile lowering is not used.
#[derive(Clone, Copy)]
pub(crate) struct Precompile {
    pub template: DefId,
    pub escape: DefId,
    pub attr: DefId,
    /// `jsxTemplateString(html)` (optional): a template without slots, one string and no arrays.
    pub template_string: Option<DefId>,
    /// `jsxList(items)` (optional): the markup of a list of rows (`list_fold`).
    pub list: Option<DefId>,
    /// `JSX.Text`: what `jsxEscape` writes into the template string.
    pub text: TyId,
}

/// What generated code calls and what JSX values are checked against.
pub(crate) struct Provider {
    /// The import source, for messages (`std/jsx`, `_ui`).
    pub source: String,
    pub jsx: DefId,
    pub fragment: DefId,
    pub component: DefId,
    pub async_component: Option<DefId>,
    pub precompile: Option<Precompile>,
    /// `jsxTextSeparator` (precompile only): markup written between adjacent text parts.
    pub text_separator: Option<String>,
    /// `jsxSoleEmpty` (precompile only): what a `null` or boolean sole child of an element
    /// renders as, instead of `jsxEscape`'s result.
    pub sole_empty: Option<String>,
    /// `jsxVoidElements`: the tags whose children are an error (they have no end tag), and that
    /// templates write without one; `None` without the export (templates use HTML's list).
    pub void_elements: Option<Vec<String>>,
    /// `JSX.IntrinsicAttributes`: attributes every component element accepts besides its props
    /// (sigx's `client:load`), passed to `jsxComponentAttributes`.
    pub component_attrs: Option<ComponentAttrs>,
    /// `JSX.Element`: the type of every JSX expression.
    pub element: TyId,
    /// `JSX.Child`: what each child is converted to.
    pub child: TyId,
    /// `JSX.AttrValue`: what each intrinsic attribute value is converted to.
    pub attr_value: TyId,
    /// `JSX.IntrinsicElements`: one field per tag.
    pub intrinsics: TyId,
    /// The props field receiving component children (`JSX.ElementChildrenAttribute`).
    pub children_field: String,
}

/// `JSX.IntrinsicAttributes` with fields besides `key` (docs/internals/contracts/jsx.md
/// "Attributes of every component").
pub(crate) struct ComponentAttrs {
    /// The exported type (an object type).
    pub ty: TyId,
    /// `jsxComponentAttributes(C, props, key, name, names, values)`.
    pub call: DefId,
}

/// The children field when the runtime has no `ElementChildrenAttribute`.
const DEFAULT_CHILDREN: &str = "children";

/// The runtime module path suffix the loader appends to the import source.
const RUNTIME_SUFFIX: &str = "/jsx-runtime";

impl FnCx<'_, '_> {
    /// The current module's JSX runtime (`None` once the reason was reported at `at`).
    pub(super) fn jsx_provider(&mut self, at: Span) -> Option<Rc<Provider>> {
        if let Some(p) = self.cx.jsx_providers.get(&self.module) {
            return p.clone();
        }
        let p = load(self.cx, self.module, at).map(Rc::new);
        self.cx.jsx_providers.insert(self.module, p.clone());
        p
    }
}

fn load(cx: &mut Ctx, m: usize, at: Span) -> Option<Provider> {
    let Some(path) = cx.modules[m].jsx_runtime.clone() else {
        cx.error(
            Diagnostic::error("no JSX runtime was loaded for this module", at)
                .with_note("the compiler driver loads `<jsxImportSource>/jsx-runtime` for modules that contain JSX"),
        );
        return None;
    };
    // A runtime that failed to load was reported by the loader.
    let t = cx.modules.iter().position(|x| x.path == path)?;
    let source = path
        .strip_suffix(RUNTIME_SUFFIX)
        .unwrap_or(&path)
        .to_string();
    let mut missing = vec![];
    let mut func = |cx: &Ctx, name: &'static str| {
        let d = function(cx, t, name);
        if d.is_none() {
            missing.push(name);
        }
        d
    };
    let (jsx, fragment, component) = (
        func(cx, "jsx"),
        func(cx, "Fragment"),
        func(cx, "jsxComponent"),
    );
    let mut ty = |cx: &mut Ctx, name: &'static str| {
        let t = type_export(cx, t, name, at);
        if t.is_none() {
            missing.push(name);
        }
        t
    };
    let element = ty(cx, "Element");
    let child = ty(cx, "Child");
    let attr_value = ty(cx, "AttrValue");
    let intrinsics = ty(cx, "IntrinsicElements");
    if !missing.is_empty() {
        report_missing(cx, &source, &missing, at);
        return None;
    }
    let children_field = children_field(cx, t, &source, at)?;
    let precompile = precompile(cx, t, at);
    let (text_separator, sole_empty) = match precompile {
        Some(_) => (
            string_const(cx, t, &source, at, "jsxTextSeparator", "<!--t-->", FOLDED)?,
            string_const(cx, t, &source, at, "jsxSoleEmpty", "", FOLDED)?,
        ),
        None => (None, None),
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
    let component_attrs = component_attrs(cx, t, &source, at)?;
    Some(Provider {
        component_attrs,
        async_component: function(cx, t, "jsxAsyncComponent"),
        precompile,
        text_separator,
        sole_empty,
        void_elements,
        source,
        jsx: jsx?,
        fragment: fragment?,
        component: component?,
        element: element?,
        child: child?,
        attr_value: attr_value?,
        intrinsics: intrinsics?,
        children_field,
    })
}

/// `IntrinsicAttributes` and `jsxComponentAttributes`: `Some(None)` without the type (or with only
/// `key`), `None` once a malformed export was reported.
fn component_attrs(
    cx: &mut Ctx,
    t: usize,
    source: &str,
    at: Span,
) -> Option<Option<ComponentAttrs>> {
    if export_of(cx, t, "IntrinsicAttributes").is_none() {
        return Some(None);
    }
    let ty = type_export(cx, t, "IntrinsicAttributes", at)?;
    let fields = match cx.ty.kind(ty).clone() {
        TyKind::Adt(d, _) => cx
            .adt(d)
            .filter(|a| matches!(a.kind, AdtKind::Anon | AdtKind::Struct))
            .map(|a| a.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>()),
        _ => None,
    };
    let Some(fields) = fields else {
        cx.error(Diagnostic::error(
            format!(
                "`JSX.IntrinsicAttributes` of the JSX provider '{source}' must be an object type"
            ),
            at,
        ));
        return None;
    };
    if fields.iter().all(|f| f == "key") {
        return Some(None);
    }
    let Some(call) = function(cx, t, "jsxComponentAttributes") else {
        cx.error(
            Diagnostic::error(
                format!("the JSX provider '{source}' declares `IntrinsicAttributes` but has no `jsxComponentAttributes`"),
                at,
            )
            .with_note("a component element with such an attribute (`<Card client:load />`) is passed to `jsxComponentAttributes(component, props, key, name, names, values)` (docs/internals/contracts/jsx.md)"),
        );
        return None;
    };
    let arity = cx.try_fn(call).map_or(0, |f| f.params.len());
    if arity != 6 {
        cx.error(Diagnostic::error(
            format!("`jsxComponentAttributes` of the JSX provider '{source}' must take 6 parameters (component, props, key, name, names, values), not {arity}"),
            at,
        ));
        return None;
    }
    Some(Some(ComponentAttrs { ty, call }))
}

/// Exported function `name` of module `t`.
fn function(cx: &Ctx, t: usize, name: &str) -> Option<DefId> {
    match export_of(cx, t, name)? {
        Item::Def(d) if cx.try_fn(d).is_some() => Some(d),
        _ => None,
    }
}

/// The type exported as `name` by module `t` (a type alias, class, struct, interface or enum).
fn type_export(cx: &mut Ctx, t: usize, name: &str, at: Span) -> Option<TyId> {
    let item = export_of(cx, t, name)?;
    if let Item::Def(d) = item {
        if cx.try_fn(d).is_some() || cx.global(d).is_some() {
            return None;
        }
    }
    let written = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: name.to_string(),
                span: at,
            }],
            args: vec![],
        },
        span: at,
    };
    Some(cx.item_type(item, name, &[], &written, &TyEnv::new(t, &[])))
}

fn precompile(cx: &mut Ctx, t: usize, at: Span) -> Option<Precompile> {
    Some(Precompile {
        template: function(cx, t, "jsxTemplate")?,
        escape: function(cx, t, "jsxEscape")?,
        attr: function(cx, t, "jsxAttr")?,
        template_string: function(cx, t, "jsxTemplateString"),
        list: function(cx, t, "jsxList"),
        text: type_export(cx, t, "Text", at)?,
    })
}

/// Why [`string_const`] needs a constant, for the precompile exports.
const FOLDED: &str = "the compiler folds it into the template strings";

/// The value of the optional string constant export `name` (`jsxTextSeparator`, `jsxSoleEmpty`,
/// `jsxVoidElements`; `example` and `why` for the message): `Some(None)` without one, `None` once
/// an export that is not a string constant was reported.
fn string_const(
    cx: &mut Ctx,
    t: usize,
    source: &str,
    at: Span,
    name: &str,
    example: &str,
    why: &str,
) -> Option<Option<String>> {
    let Some(item) = export_of(cx, t, name) else {
        return Some(None);
    };
    let mut value = None;
    if let Item::Def(mut d) = item {
        // `export const jsxTextSeparator = OTHER;` reads `OTHER` (a bounded chain).
        for _ in 0..16 {
            if cx.global(d).is_none() {
                break;
            }
            crate::body::driver::ensure_global(cx, d);
            let Some(g) = cx.global(d) else { break };
            match g.init.as_ref().map(|i| &i.kind) {
                Some(H::Lit(Lit::Str(s))) => value = Some(s.clone()),
                Some(H::Global(next)) => {
                    d = *next;
                    continue;
                }
                _ => {
                    if let TyKind::Literal(LitValue::Str(s)) = cx.ty.kind(g.ty) {
                        value = Some(s.clone());
                    }
                }
            }
            break;
        }
    }
    if value.is_none() {
        cx.error(
            Diagnostic::error(
                format!("`{name}` of the JSX provider '{source}' must be a string constant"),
                at,
            )
            .with_note(format!(
                "write it as `export const {name} = \"{example}\";`: {why} (docs/internals/contracts/jsx.md)"
            )),
        );
        return None;
    }
    Some(value)
}

/// The one field name of `ElementChildrenAttribute`, or `children` without one.
fn children_field(cx: &mut Ctx, t: usize, source: &str, at: Span) -> Option<String> {
    let Some(ty) = type_export(cx, t, "ElementChildrenAttribute", at) else {
        return Some(DEFAULT_CHILDREN.to_string());
    };
    if let TyKind::Adt(d, _) = cx.ty.kind(ty) {
        if let Some([f]) = cx.adt(*d).map(|a| a.fields.as_slice()) {
            return Some(f.name.clone());
        }
    }
    cx.error(
        Diagnostic::error(
            format!("`JSX.ElementChildrenAttribute` of the JSX provider '{source}' must be an object type with one field"),
            at,
        )
        .with_note("its field names the props field that receives component children, e.g. `{ children: {} }`"),
    );
    None
}

fn report_missing(cx: &mut Ctx, source: &str, missing: &[&str], at: Span) {
    let list: Vec<String> = missing.iter().map(|n| format!("`{n}`")).collect();
    cx.error(
        Diagnostic::error(
            format!(
                "the JSX provider '{source}' does not export {}",
                list.join(", ")
            ),
            at,
        )
        .with_note(format!(
            "`{source}{RUNTIME_SUFFIX}` must export the types `Element`, `Child`, `AttrValue` and `IntrinsicElements` and the functions `jsx`, `Fragment` and `jsxComponent` (docs/internals/contracts/jsx.md)"
        )),
    );
}
