//! JSX elements and fragments: `<div class="a" {...p}>text {x}<br /></div>`, `<>…</>`,
//! member (`ui.Card`) and namespaced (`svg:rect`) names, attribute values (string, `{expr}`,
//! element), children (text, `{expr}`, `{}` / `{/* comment */}`, `{...spread}`, elements).
//!
//! The lexer has already split JSX into its own tokens (`JsxLt`, `JsxIdent`, `JsxText`, ...), so
//! this is plain recursive descent. Closing-tag diagnostics are worded like TypeScript's.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::Tok;
use velt_common::Span;

impl Parser<'_> {
    /// An element or fragment; the cursor is at `JsxLt`.
    pub(super) fn parse_jsx_element(&mut self) -> PResult<JsxElement> {
        self.guarded(|p| p.jsx_element_inner())
    }

    fn jsx_element_inner(&mut self) -> PResult<JsxElement> {
        let lo = self.cur_lo();
        let open_span = self.cur_span();
        self.bump(); // <
        if self.eat(Tok::JsxGt) {
            let children = self.jsx_children(None, open_span)?;
            return Ok(JsxElement {
                name: None,
                attrs: vec![],
                children,
                span: self.span_from(lo),
            });
        }
        let name = self.jsx_name()?;
        let mut attrs = Vec::new();
        while !matches!(self.peek(), Tok::JsxGt | Tok::JsxSlashGt | Tok::Eof) {
            attrs.push(self.jsx_attr()?);
        }
        let children = if self.eat(Tok::JsxSlashGt) {
            vec![]
        } else {
            self.expect(Tok::JsxGt)?;
            let name_span = name.span();
            self.jsx_children(Some(&name), name_span)?
        };
        Ok(JsxElement {
            name: Some(name),
            attrs,
            children,
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
    /// fragment); `open_span` is where an unclosed element is reported.
    fn jsx_children(&mut self, name: Option<&JsxName>, open_span: Span) -> PResult<Vec<JsxChild>> {
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
                    self.jsx_closing_tag(name)?;
                    return Ok(children);
                }
                _ => {
                    let msg = match name {
                        Some(n) => format!(
                            "JSX element '{}' has no corresponding closing tag.",
                            n.to_source()
                        ),
                        None => "JSX fragment has no corresponding closing tag.".to_string(),
                    };
                    self.error(msg, open_span);
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
    fn jsx_closing_tag(&mut self, name: Option<&JsxName>) -> PResult<()> {
        let lo = self.cur_lo();
        self.bump(); // </
        let closing = if self.at(Tok::JsxGt) {
            None
        } else {
            Some(self.jsx_name()?)
        };
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
        Ok(())
    }
}
