//! The type after `as` / `satisfies`: a whole type, with speculative generic arguments, and
//! the number literal members it rejects.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};

impl<'a> Parser<'a> {
    /// Type after `as` or `satisfies`: a whole type, as in TypeScript, so `x as A | B` is a
    /// union (a leading `|` is allowed) and `x as A & B` an intersection. Generic arguments of a
    /// named type are speculative, so `a as i64 < b` stays a comparison.
    pub(super) fn parse_cast_type(&mut self) -> PResult<TypeExpr> {
        if self.at_kw(Kw::Const) {
            // `as const`: the type named `const` (sema keeps the value as it is).
            let span = self.cur_span();
            self.bump();
            return Ok(TypeExpr {
                kind: TypeExprKind::Named {
                    path: vec![Ident {
                        name: "const".into(),
                        span,
                    }],
                    args: vec![],
                },
                span,
            });
        }
        self.guarded(|p| {
            let lo = p.cur_lo();
            p.eat(Tok::Pipe);
            let first = p.parse_cast_intersection()?;
            if !p.at(Tok::Pipe) {
                return Ok(first);
            }
            let mut members = vec![first];
            while p.eat(Tok::Pipe) {
                let member = p.parse_cast_intersection()?;
                p.reject_cast_number_member(&member);
                members.push(member);
            }
            Ok(TypeExpr {
                kind: TypeExprKind::Union(members),
                span: p.span_from(lo),
            })
        })
    }

    /// `A & B & …` after `as` (a leading `&` is allowed), or one member type.
    fn parse_cast_intersection(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        self.eat(Tok::Amp);
        let first = self.parse_cast_member()?;
        if !self.at(Tok::Amp) {
            return Ok(first);
        }
        let mut members = vec![first];
        while self.eat(Tok::Amp) {
            members.push(self.parse_cast_member()?);
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Intersection(members),
            span: self.span_from(lo),
        })
    }

    /// One member of the type after `as`: a named type (with speculative generic arguments)
    /// and its `[]` suffixes, or any other type without a top-level union.
    fn parse_cast_member(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        if self.at_ident_like() && !self.at_keyof() && !self.at_word("undefined") {
            let named = self.parse_named_type_with(true)?;
            let ty = self.parse_array_suffixes(lo, named);
            return self.parse_indexed_suffixes(lo, ty);
        }
        self.parse_type_no_union()
    }

    /// A numeric literal type after `|` in an `as` type (`x as i32 | 1`): TypeScript reads a
    /// union there, and Velt read a bitwise or before; rather than change such a program's
    /// value silently, it is an error that shows both spellings.
    pub(super) fn reject_cast_number_member(&mut self, member: &TypeExpr) {
        let TypeExprKind::Literal(SignedLit {
            lit: Lit::Int { .. } | Lit::Float { .. },
            ..
        }) = &member.kind
        else {
            return;
        };
        if self.speculating != 0 {
            return;
        }
        self.diags.push(
            velt_common::Diagnostic::error(
                "a cast to a union with a number literal member is ambiguous",
                member.span,
            )
            .with_note(
                "write `(x as T) | n` for a bitwise or of the cast value, or `x as (T | n)` for a cast to the union type",
            ),
        );
    }
}
