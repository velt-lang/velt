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
            Lit::Undefined => "undefined".into(),
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
                let method = value.span.lo <= key.span.lo;
                let doc = self.prop_key(key);
                match &value.kind {
                    // `name(params) { body }`, which the parser gives as an arrow property whose
                    // span starts with the name (an arrow written after `name:` starts later).
                    ExprKind::Arrow {
                        type_params,
                        params,
                        ret,
                        throws,
                        body,
                        is_async,
                    } if method => self.arrow(
                        type_params,
                        params,
                        (ret.as_ref(), throws.as_ref()),
                        body,
                        *is_async,
                        Some(doc),
                    ),
                    // `{ a = 1 }` (a shorthand default in an assignment target), which the
                    // parser gives as `a: a = 1` with the assignment starting at the key.
                    ExprKind::Assign {
                        op: None,
                        target,
                        value: dflt,
                    } if value.span.lo == key.span.lo
                        && matches!(&target.kind, ExprKind::Ident(t) if t.name == key.name) =>
                    {
                        cat![doc, " = ", self.expr(dflt)]
                    }
                    // `*name(params) { body }` / `async *name()`, which the parser gives as a
                    // property holding a generator function whose span starts before the name.
                    ExprKind::Function(f) if method => {
                        let raw = slice(self.src, key.span);
                        let name = match raw.starts_with(['"', '\'']) {
                            true => string_literal(raw),
                            false => key.name.clone(),
                        };
                        let mut f = (**f).clone();
                        f.sig.name = Ident {
                            name,
                            span: key.span,
                        };
                        self.fn_decl(&f, "")
                    }
                    // `get name() { body }` / `set name(v) { body }`, which the parser gives as
                    // a call of `__object_get` / `__object_set` (callee span: the keyword) of
                    // the accessor as an arrow.
                    ExprKind::Call { callee, args, .. } if self.accessor_kw(callee).is_some() => {
                        let kw = self.accessor_kw(callee).expect("ICE: checked above");
                        match args.as_slice() {
                            [Expr {
                                kind:
                                    ExprKind::Arrow {
                                        type_params,
                                        params,
                                        ret,
                                        throws,
                                        body,
                                        is_async,
                                    },
                                ..
                            }] => self.arrow(
                                type_params,
                                params,
                                (ret.as_ref(), throws.as_ref()),
                                body,
                                *is_async,
                                Some(cat![kw, " ", doc]),
                            ),
                            _ => self.property(doc, value),
                        }
                    }
                    _ => self.property(doc, value),
                }
            }
            ObjectProp::Shorthand(key) => self.prop_key(key),
            ObjectProp::Spread(value) => match self.computed_entry(value) {
                // `[key]: value`, which the parser gives as a spread of a call of
                // `__computed_key` whose callee's span is the bracketed key.
                Some((key, v)) => {
                    let key = cat!["[", self.expr(key), "]"];
                    self.property(key, v)
                }
                None => cat!["...", self.expr(value)],
            },
            ObjectProp::Method(f) => self.fn_decl(f, ""),
        }
    }

    /// `"get"` / `"set"` if `callee` is the `__object_get` / `__object_set` of an accessor
    /// (its span is the keyword, not a written name).
    fn accessor_kw(&self, callee: &Expr) -> Option<&'static str> {
        let ExprKind::Ident(f) = &callee.kind else {
            return None;
        };
        let raw = slice(self.src, callee.span);
        match f.name.as_str() {
            velt_syntax::ast::OBJECT_GET_FN if raw == "get" => Some("get"),
            velt_syntax::ast::OBJECT_SET_FN if raw == "set" => Some("set"),
            _ => None,
        }
    }

    /// The key and value of `[key]: value` (a computed key known only at run time), if `e` is
    /// the call the parser writes it as (not a written `__computed_key(...)`).
    fn computed_entry<'e>(&self, e: &'e Expr) -> Option<(&'e Expr, &'e Expr)> {
        let ExprKind::Call { callee, args, .. } = &e.kind else {
            return None;
        };
        match (&callee.kind, args.as_slice()) {
            (ExprKind::Ident(f), [key, value])
                if f.name == velt_syntax::ast::COMPUTED_KEY_FN
                    && slice(self.src, callee.span).starts_with('[') =>
            {
                Some((key, value))
            }
            _ => None,
        }
    }

    /// `key: value`, breaking after the colon like an assignment when the value is long.
    fn property(&mut self, key: Doc, value: &Expr) -> Doc {
        self.assignment(key, ":", value)
    }

    /// Was property name `key` written as a string literal (`"a-b"`) or a template (`` [`a-b`] ``)?
    pub(super) fn quoted(&self, key: &Ident) -> bool {
        slice(self.src, key.span).starts_with(['"', '\'', '`'])
    }

    /// A property name as written: identifier, string literal (in house quotes), or a template
    /// without substitutions in its brackets (`` [`a-b`] ``, kept as written: its escapes are in
    /// the source only), with the brackets of a string or number literal written in brackets
    /// (`["k"]: T`, `[7]: 1`), which the parser gives as the plain name.
    pub(super) fn prop_key(&self, key: &Ident) -> Doc {
        let before = self.src[..key.span.lo as usize].trim_end();
        let after = self.src[key.span.hi as usize..].trim_start();
        let template = slice(self.src, key.span).starts_with('`');
        if before.ends_with('[')
            && after.starts_with(']')
            && !key.name.starts_with('[')
            && !template
        {
            return cat!["[", self.plain_prop_key(key), "]"];
        }
        self.plain_prop_key(key)
    }

    /// [`Self::prop_key`] without the brackets.
    fn plain_prop_key(&self, key: &Ident) -> Doc {
        let raw = slice(self.src, key.span);
        let printed = if raw.starts_with(['"', '\'']) {
            string_literal(raw)
        } else if raw.starts_with('`') {
            format!("[{raw}]")
        } else if raw.starts_with(|c: char| c.is_ascii_digit()) {
            // A numeric name (`7`, `0x10`) as written.
            raw.to_string()
        } else {
            key.name.clone()
        };
        text(printed)
    }
}

fn prop_range(prop: &ObjectProp) -> (u32, u32) {
    match prop {
        ObjectProp::KeyValue(key, value) => (key.span.lo, value.span.hi),
        ObjectProp::Shorthand(key) => (key.span.lo, key.span.hi),
        ObjectProp::Spread(value) => (value.span.lo, value.span.hi),
        ObjectProp::Method(f) => (f.sig.span.lo, f.body.span.hi),
    }
}

fn is_multi_entry_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Object(props) => props.len() > 1,
        ExprKind::Array(elems) => elems.len() > 1,
        _ => false,
    }
}
