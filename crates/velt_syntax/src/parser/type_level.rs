//! Mapped types (`{ readonly [K in S as N]?: V }`) and template literal types (`` `on${K}` ``),
//! and conditional types (`C extends E ? T : F`, at the top of `parse_type`), issue #934.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok, TplPart};

impl Parser<'_> {
    /// Full type including unions: `A | B | null` (a leading `|` is allowed), whose members may
    /// be intersections (`A & B | C` is `(A & B) | C`).
    pub(super) fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            let check = p.parse_union_type()?;
            if !p.at_kw(Kw::Extends) {
                return Ok(check);
            }
            // `C extends E ? T : F`. As in TypeScript, `E` is parsed without a conditional
            // type of its own (`A extends B extends C ? …` needs parentheses), and the
            // branches nest to the right.
            p.bump(); // extends
            let extends = p.parse_union_type()?;
            p.expect(Tok::Question)?;
            let then = p.parse_type()?;
            p.expect(Tok::Colon)?;
            let else_ = p.parse_type()?;
            Ok(TypeExpr {
                kind: TypeExprKind::Conditional(Box::new(ConditionalType {
                    check,
                    extends,
                    then,
                    else_,
                })),
                span: p.span_from(lo),
            })
        })
    }

    /// After `{`: `[K in`, optionally after `readonly`, `+readonly` or `-readonly`?
    pub(super) fn at_mapped_type(&mut self) -> bool {
        let mut i = usize::from(matches!(self.peek(), Tok::Plus | Tok::Minus));
        if self.nth(i) == Tok::Kw(Kw::Readonly) {
            i += 1;
        } else if i == 1 {
            return false;
        }
        self.nth(i) == Tok::LBracket
            && Self::is_name(self.nth(i + 1))
            && self.nth(i + 2) == Tok::Kw(Kw::In)
    }

    /// `{ readonly [K in S as N]?: V }` after the `{` (from `lo`).
    pub(super) fn parse_mapped_type(&mut self, lo: u32) -> PResult<TypeExpr> {
        let readonly = self.mapped_modifier(|p| p.at_kw(Kw::Readonly));
        self.expect(Tok::LBracket)?;
        let param = self.take_ident();
        self.bump(); // in
        let source = self.parse_type()?;
        let name = match self.eat_kw(Kw::As) {
            true => Some(self.parse_type()?),
            false => None,
        };
        self.expect(Tok::RBracket)?;
        let optional = self.mapped_modifier(|p| p.at(Tok::Question));
        self.expect(Tok::Colon)?;
        let value = self.parse_type()?;
        if !self.eat(Tok::Semi) {
            self.eat(Tok::Comma);
        }
        self.expect(Tok::RBrace)?;
        Ok(TypeExpr {
            kind: TypeExprKind::Mapped(Box::new(MappedType {
                param,
                source,
                name,
                readonly,
                optional,
                value,
            })),
            span: self.span_from(lo),
        })
    }

    /// A mapped type's modifier: `+m`, `-m`, `m` or nothing, where `at` finds `m`.
    fn mapped_modifier(&mut self, at: impl Fn(&mut Self) -> bool) -> MappedModifier {
        let sign = match self.peek() {
            Tok::Plus => MappedModifier::Add,
            Tok::Minus => MappedModifier::Remove,
            _ if at(self) => {
                self.bump();
                return MappedModifier::Add;
            }
            _ => return MappedModifier::Keep,
        };
        self.bump(); // + or -
        if at(self) {
            self.bump();
        } else {
            self.error_expected("`readonly` or `?`");
        }
        sign
    }

    /// `` `a${T}b` `` in a type: a template literal type.
    /// `infer X` (at `infer`, starting at `lo`).
    pub(super) fn parse_infer_type(&mut self, lo: u32) -> PResult<TypeExpr> {
        self.bump(); // infer
        let name = self.parse_ident()?;
        Ok(TypeExpr {
            kind: TypeExprKind::Infer(name),
            span: self.span_from(lo),
        })
    }

    pub(super) fn parse_template_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let Tok::Template(idx, part) = self.peek() else {
            self.error_expected("template literal type");
            return Err(Fail);
        };
        self.bump();
        let mut quasis = vec![self.payload_text(idx)];
        let mut types = Vec::new();
        if part == TplPart::Head {
            loop {
                types.push(self.parse_type()?);
                let Tok::Template(idx, part @ (TplPart::Middle | TplPart::Tail)) = self.peek()
                else {
                    self.error_expected("`}`");
                    return Err(Fail);
                };
                quasis.push(self.payload_text(idx));
                self.bump();
                if part == TplPart::Tail {
                    break;
                }
            }
        }
        Ok(TypeExpr {
            kind: TypeExprKind::TemplateLiteral { quasis, types },
            span: self.span_from(lo),
        })
    }
}
