//! JSX elements and fragments: `<div class="a" {...p}>text {x}<br /></div>`, `<>…</>`,
//! member (`ui.Card`) and namespaced (`svg:rect`) names, type arguments (`<List<number> …>`),
//! attribute values (string, `{expr}`, element), children (text, `{expr}`, `{}` /
//! `{/* comment */}`, `{...spread}`, elements).
//!
//! The parser decides where an element starts: at a `<` where it expects an expression (after
//! ruling out a generic arrow `<T>(x: T) => x`, see `arrow`), it has the lexer re-lex from that
//! `<` in JSX mode. From there the lexer splits JSX into its own tokens (`JsxLt`, `JsxIdent`,
//! `JsxText`, ...), so this is plain recursive descent. Closing-tag diagnostics are worded like
//! TypeScript's.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{may_start_jsx, Tok};
use velt_common::{Diagnostic, Span};

/// Note on an unclosed element that looks like a generic arrow whose head did not parse.
const GENERIC_ARROW_NOTE: &str = "if this is a generic arrow function, check its parameter list and return type: `<T>(x: T): T => x`";

impl Parser<'_> {
    /// Can the `<` at the cursor start an element (`<name` or the fragment `<>`)?
    pub(super) fn jsx_starts_here(&mut self) -> bool {
        let at = self.cur_lo() as usize + 1;
        self.src
            .as_bytes()
            .get(at)
            .is_some_and(|&c| may_start_jsx(c))
    }

    /// An element or fragment; the cursor is at `JsxLt`.
    pub(super) fn parse_jsx_element(&mut self) -> PResult<JsxElement> {
        self.guarded(|p| p.jsx_element_inner())
    }

    fn jsx_element_inner(&mut self) -> PResult<JsxElement> {
        let lo = self.cur_lo();
        let open_span = self.cur_span();
        self.bump(); // <
        if self.eat(Tok::JsxGt) {
            let (children, _) = self.jsx_children(None, open_span, false)?;
            return Ok(JsxElement {
                name: None,
                type_args: vec![],
                attrs: vec![],
                children,
                closing_name: None,
                span: self.span_from(lo),
            });
        }
        let name = self.jsx_name()?;
        let type_args = if self.at(Tok::Lt) {
            self.parse_type_args()?
        } else {
            vec![]
        };
        let mut attrs = Vec::new();
        while !matches!(self.peek(), Tok::JsxGt | Tok::JsxSlashGt | Tok::Eof) {
            attrs.push(self.jsx_attr()?);
        }
        let (children, closing_name) = if self.eat(Tok::JsxSlashGt) {
            (vec![], None)
        } else {
            self.expect(Tok::JsxGt)?;
            let name_span = name.span();
            // `<T>(x: T => x`: a broken generic arrow ends up here.
            let arrow_like =
                attrs.is_empty() && type_args.is_empty() && matches!(name, JsxName::Ident(_));
            self.jsx_children(Some(&name), name_span, arrow_like)?
        };
        Ok(JsxElement {
            name: Some(name),
            type_args,
            attrs,
            children,
            closing_name,
            span: self.span_from(lo),
        })
    }

    /// `div`, `my-el`, `ui.Card`, `svg:rect`.
    fn jsx_name(&mut self) -> PResult<JsxName> {
        let first = self.jsx_ident()?;
        if self.eat(Tok::Colon) {
            return Ok(JsxName::Namespaced(first, self.jsx_ident()?));
        }
        if !self.at(Tok::Dot) {
            return Ok(JsxName::Ident(first));
        }
        let mut parts = vec![first];
        while self.eat(Tok::Dot) {
            parts.push(self.jsx_ident()?);
        }
        Ok(JsxName::Member(parts))
    }

    fn jsx_ident(&mut self) -> PResult<Ident> {
        if self.at(Tok::JsxIdent) {
            return Ok(self.take_ident());
        }
        self.error_expected("JSX identifier");
        Err(Fail)
    }

    /// `name`, `name=value`, `ns:name=value` or `{...expr}`.
    fn jsx_attr(&mut self) -> PResult<JsxAttr> {
        let lo = self.cur_lo();
        if self.eat(Tok::LBrace) {
            self.expect(Tok::DotDotDot)?;
            let expr = self.parse_assign()?;
            self.expect(Tok::RBrace)?;
            let span = self.span_from(lo);
            return Ok(JsxAttr::Spread { expr, span });
        }
        if !self.at(Tok::JsxIdent) {
            self.error_expected("JSX attribute");
            return Err(Fail);
        }
        let first = self.take_ident();
        let name = if self.eat(Tok::Colon) {
            JsxAttrName::Namespaced(first, self.jsx_ident()?)
        } else {
            JsxAttrName::Ident(first)
        };
        let value = if self.eat(Tok::Eq) {
            self.jsx_attr_value()?
        } else {
            None
        };
        let span = self.span_from(lo);
        Ok(JsxAttr::Named { name, value, span })
    }

    /// `"text"`, `{expr}` or an element after `=`. `{}` is reported and dropped.
    fn jsx_attr_value(&mut self) -> PResult<Option<JsxAttrValue>> {
        let lo = self.cur_lo();
        match self.peek() {
            Tok::Str(idx) => {
                let span = self.cur_span();
                self.bump();
                let value = self.payload_text(idx);
                Ok(Some(JsxAttrValue::Str { value, span }))
            }
            Tok::LBrace => {
                self.bump();
                if self.eat(Tok::RBrace) {
                    let span = self.span_from(lo);
                    self.error(
                        "JSX attributes must only be assigned a non-empty expression.",
                        span,
                    );
                    return Ok(None);
                }
                let expr = self.parse_assign()?;
                self.expect(Tok::RBrace)?;
                let span = self.span_from(lo);
                Ok(Some(JsxAttrValue::Expr { expr, span }))
            }
            Tok::JsxLt => Ok(Some(JsxAttrValue::Element(self.parse_jsx_element()?))),
            _ => {
                self.error_expected("JSX attribute value");
                Err(Fail)
            }
        }
    }

    /// Children up to and including the closing tag of the element named `name` (`None` for a
    /// fragment); `open_span` is where an unclosed element is reported. `arrow_like`: the opening
    /// tag could have been the type parameters of a generic arrow. Also returns the closing tag's
    /// name.
    fn jsx_children(
        &mut self,
        name: Option<&JsxName>,
        open_span: Span,
        arrow_like: bool,
    ) -> PResult<(Vec<JsxChild>, Option<JsxName>)> {
        let mut children = Vec::new();
        loop {
            match self.peek() {
                Tok::JsxText(idx) => {
                    let span = self.cur_span();
                    self.bump();
                    let value = self.payload_text(idx);
                    if !value.is_empty() {
                        children.push(JsxChild::Text { value, span });
                    }
                }
                Tok::LBrace => children.push(self.jsx_child_container()?),
                Tok::JsxLt => children.push(JsxChild::Element(self.parse_jsx_element()?)),
                Tok::JsxLtSlash => {
                    let closing = self.jsx_closing_tag(name)?;
                    return Ok((children, closing));
                }
                _ => {
                    let msg = match name {
                        Some(n) => format!(
                            "JSX element '{}' has no corresponding closing tag.",
                            n.to_source()
                        ),
                        None => "JSX fragment has no corresponding closing tag.".to_string(),
                    };
                    let looks_like_arrow = arrow_like
                        && matches!(children.first(), Some(JsxChild::Text { value, .. })
                            if value.starts_with('(') && value.contains("=>"));
                    let mut diag = Diagnostic::error(msg, open_span);
                    if looks_like_arrow {
                        diag = diag.with_note(GENERIC_ARROW_NOTE);
                    }
                    if self.speculating == 0 {
                        self.diags.push(diag);
                    }
                    return Err(Fail);
                }
            }
        }
    }

    /// `{expr}`, `{}` (also `{/* comment */}`) or `{...expr}` among the children.
    fn jsx_child_container(&mut self) -> PResult<JsxChild> {
        let lo = self.cur_lo();
        self.bump(); // {
        if self.eat(Tok::RBrace) {
            let span = self.span_from(lo);
            return Ok(JsxChild::Expr { expr: None, span });
        }
        let spread = self.eat(Tok::DotDotDot);
        let expr = if spread {
            self.parse_assign()?
        } else {
            self.parse_expr()?
        };
        self.expect(Tok::RBrace)?;
        let span = self.span_from(lo);
        Ok(if spread {
            JsxChild::Spread { expr, span }
        } else {
            JsxChild::Expr {
                expr: Some(expr),
                span,
            }
        })
    }

    /// `</name>` or `</>`, checked against the opening `name`; the cursor is at `JsxLtSlash`.
    /// Returns the closing name.
    fn jsx_closing_tag(&mut self, name: Option<&JsxName>) -> PResult<Option<JsxName>> {
        let lo = self.cur_lo();
        self.bump(); // </
        let closing = if self.at(Tok::JsxGt) {
            None
        } else {
            Some(self.jsx_name()?)
        };
        if let (Some(close), true) = (&closing, self.at(Tok::Lt)) {
            self.closing_type_args(close)?;
        }
        self.expect(Tok::JsxGt)?;
        let span = closing
            .as_ref()
            .map_or_else(|| self.span_from(lo), JsxName::span);
        match (name, &closing) {
            (Some(open), Some(close)) if open.to_source() == close.to_source() => {}
            (None, None) => {}
            (Some(open), _) => self.error(
                format!(
                    "Expected corresponding JSX closing tag for '{}'.",
                    open.to_source()
                ),
                span,
            ),
            (None, Some(_)) => {
                self.error("Expected corresponding closing tag for JSX fragment.", span)
            }
        }
        Ok(closing)
    }

    /// `</List<T>>`: reported (type arguments go on the opening tag) and skipped.
    fn closing_type_args(&mut self, close: &JsxName) -> PResult<()> {
        let lo = self.cur_lo();
        self.parse_type_args()?;
        if self.speculating == 0 {
            let d = Diagnostic::error("a closing tag takes no type arguments", self.span_from(lo))
                .with_note(format!(
                    "they go on the opening tag only; close it with `</{}>`",
                    close.to_source()
                ));
            self.diags.push(d);
        }
        Ok(())
    }
}
