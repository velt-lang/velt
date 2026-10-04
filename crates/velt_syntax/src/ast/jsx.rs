//! JSX nodes of the surface AST (part of the `ast` contract): elements, fragments, attributes
//! and children, exactly as written in TSX. Text and attribute strings are stored cooked (JSX
//! whitespace rules applied, HTML entities decoded); their spans cover the raw source.

use velt_common::Span;

use super::{Expr, Ident, TypeExpr};

/// `<name attrs>children</name>`, `<name attrs />`, or a fragment `<>children</>`.
#[derive(Clone, Debug)]
pub struct JsxElement {
    /// `None` for a fragment.
    pub name: Option<JsxName>,
    /// Explicit type arguments of the opening tag (`<List<number> …>`); usually empty.
    pub type_args: Vec<TypeExpr>,
    pub attrs: Vec<JsxAttr>,
    /// Empty for a self-closing element.
    pub children: Vec<JsxChild>,
    /// The name in the closing tag, as written (`Card` in `</Card>`, with its own span); `None`
    /// for a self-closing element and a fragment. In a tree recovered from errors it may differ
    /// from `name` (`<A></B>`, which the parser reports); it names something only when it is
    /// spelled like `name`.
    pub closing_name: Option<JsxName>,
    pub span: Span,
}

/// Tag name of an element.
#[derive(Clone, Debug)]
pub enum JsxName {
    /// `div`, `my-element`, `Card` (may contain `-`).
    Ident(Ident),
    /// `ui.Card`, `a.b.c` (at least two parts).
    Member(Vec<Ident>),
    /// `svg:rect`
    Namespaced(Ident, Ident),
}

/// Name of a named attribute.
#[derive(Clone, Debug)]
pub enum JsxAttrName {
    /// `class`, `data-id`, `for` (keywords are plain names here).
    Ident(Ident),
    /// `xlink:href`
    Namespaced(Ident, Ident),
}

/// One attribute of an opening tag.
#[derive(Clone, Debug)]
pub enum JsxAttr {
    /// `name`, `name="text"`, `name={expr}`, `name=<el />`; `value: None` for a bare `name`.
    Named {
        name: JsxAttrName,
        value: Option<JsxAttrValue>,
        span: Span,
    },
    /// `{...expr}`; `span` includes the braces.
    Spread { expr: Expr, span: Span },
}

/// Value of a named attribute.
#[derive(Clone, Debug)]
pub enum JsxAttrValue {
    /// `"text"` / `'text'`: no backslash escapes, HTML entities decoded; `span` includes the
    /// quotes.
    Str { value: String, span: Span },
    /// `{expr}`; `span` includes the braces.
    Expr { expr: Expr, span: Span },
    /// `<el />` (an element or fragment directly as the value).
    Element(JsxElement),
}

/// One child of an element or fragment.
#[derive(Clone, Debug)]
pub enum JsxChild {
    /// Text after the JSX whitespace rules (lines trimmed, line breaks between non-empty lines
    /// become one space, whitespace-only lines dropped) with entities decoded; never empty.
    /// `span` covers the raw text.
    Text {
        value: String,
        span: Span,
    },
    /// `{expr}`, or `{}` / `{/* comment */}` (`expr: None`); `span` includes the braces.
    Expr {
        expr: Option<Expr>,
        span: Span,
    },
    /// `{...expr}`; `span` includes the braces.
    Spread {
        expr: Expr,
        span: Span,
    },
    Element(JsxElement),
}

impl JsxName {
    /// The name as written: `div`, `ui.Card`, `svg:rect`.
    pub fn to_source(&self) -> String {
        match self {
            JsxName::Ident(id) => id.name.clone(),
            JsxName::Member(parts) => parts
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            JsxName::Namespaced(ns, name) => format!("{}:{}", ns.name, name.name),
        }
    }

    /// Span of the whole name.
    pub fn span(&self) -> Span {
        match self {
            JsxName::Ident(id) => id.span,
            JsxName::Member(parts) => match (parts.first(), parts.last()) {
                (Some(first), Some(last)) => first.span.to(last.span),
                _ => Span::DUMMY,
            },
            JsxName::Namespaced(ns, name) => ns.span.to(name.span),
        }
    }
}

impl JsxAttrName {
    /// The name as written: `class`, `xlink:href`.
    pub fn to_source(&self) -> String {
        match self {
            JsxAttrName::Ident(id) => id.name.clone(),
            JsxAttrName::Namespaced(ns, name) => format!("{}:{}", ns.name, name.name),
        }
    }
}
