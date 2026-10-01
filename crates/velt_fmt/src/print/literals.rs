//! Literals: scalars (spelled as in the source, strings in double quotes), arrays, object and
//! struct literals (`{ a, b: 1, ...rest }` with spaces inside the braces).

use velt_common::Span;
use velt_syntax::ast::{Expr, ExprKind, Ident, Lit, ObjectProp, TypeExpr};

use super::lists::{delimited, delimited_with};
use super::Printer;
use crate::doc::{cat, group, text, Doc, Node};
use crate::source::{slice, string_literal};

impl<'a> Printer<'a> {
    pub(super) fn lit(&mut self, lit: &Lit, span: Span) -> Doc {
        match lit {
            Lit::Str(_) => text(string_literal(slice(self.src, span))),
            Lit::Int { .. } | Lit::Float { .. } => text(slice(self.src, span)),
            Lit::Bool(true) => "true".into(),
            Lit::Bool(false) => "false".into(),
            Lit::Null => "null".into(),
        }
    }

    /// `[a, b]`; an array of several non-trivial objects/arrays is always one per line.
    pub(super) fn array(&mut self, elems: &[Expr], end: u32) -> Doc {
        let list = self.list(elems, end, |e| (e.span.lo, e.span.hi), |p, e| p.expr(e));
        let force = elems.len() > 1 && elems.iter().all(is_multi_entry_literal);
        delimited_with("[", list, "]", false, force)
    }

    pub(super) fn object(&mut self, props: &[ObjectProp], end: u32) -> Doc {
        let list = self.list(props, end, prop_range, |p, prop| p.object_prop(prop));
        delimited("{", list, "}", true)
    }

    /// `Name { props }`, one group so that hugging can expand it as a whole.
    pub(super) fn struct_lit(&mut self, name: &TypeExpr, props: &[ObjectProp], end: u32) -> Doc {
        let name = self.ty(name);
        let body = self.object(props, end);
        match body.node() {
            Node::Group { contents, .. } => group(cat![name, " ", contents.clone()]),
            _ => cat![name, " ", body],
        }
    }

    fn object_prop(&mut self, prop: &ObjectProp) -> Doc {
        match prop {
            ObjectProp::KeyValue(key, value) => {
                let key = self.prop_key(key);
                self.property(key, value)
            }
            ObjectProp::Shorthand(key) => self.prop_key(key),
            ObjectProp::Spread(value) => cat!["...", self.expr(value)],
        }
    }

    /// `key: value`, breaking after the colon like an assignment when the value is long.
    fn property(&mut self, key: Doc, value: &Expr) -> Doc {
        self.assignment(key, ":", value)
    }

    /// A property name as written: identifier, or string literal (in house quotes).
    fn prop_key(&self, key: &Ident) -> Doc {
        let raw = slice(self.src, key.span);
        if raw.starts_with(['"', '\'']) {
            text(string_literal(raw))
        } else {
            text(key.name.clone())
        }
    }
}

fn prop_range(prop: &ObjectProp) -> (u32, u32) {
    match prop {
        ObjectProp::KeyValue(key, value) => (key.span.lo, value.span.hi),
        ObjectProp::Shorthand(key) => (key.span.lo, key.span.hi),
        ObjectProp::Spread(value) => (value.span.lo, value.span.hi),
    }
}

fn is_multi_entry_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Object(props) => props.len() > 1,
        ExprKind::Array(elems) => elems.len() > 1,
        _ => false,
    }
}
