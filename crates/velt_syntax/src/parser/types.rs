//! Type expressions: named types with generic arguments, `T[]`, tuples, function types, unions,
//! literal types (`"circle"`, `-1`, `true`), object types (`{ kind: "circle"; r: f64 }`), `null`
//! and `void`; `name?: T` (optional parameters and object-type fields) as `T | null`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// Full type including unions: `A | B | null` (a leading `|` is allowed).
    pub(super) fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            p.eat(Tok::Pipe);
            let first = p.parse_type_no_union()?;
            if !p.at(Tok::Pipe) {
                return Ok(first);
            }
            let mut members = vec![first];
            while p.eat(Tok::Pipe) {
                members.push(p.parse_type_no_union()?);
            }
            Ok(TypeExpr {
                kind: TypeExprKind::Union(members),
                span: p.span_from(lo),
            })
        })
    }

    /// Type after `as`. Unions are limited to a trailing `| null` so `x as u8 | y` stays a bit-or,
    /// and generic arguments are speculative so `a as i64 < b` stays a comparison.
    pub(super) fn parse_cast_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let mut ty = if self.at_ident_like() {
            let named = self.parse_named_type_with(true)?;
            self.parse_array_suffixes(lo, named)
        } else {
            self.parse_type_no_union()?
        };
        if self.at(Tok::Pipe) && self.nth(1) == Tok::Kw(Kw::Null) {
            let mut members = vec![ty];
            while self.at(Tok::Pipe) && self.nth(1) == Tok::Kw(Kw::Null) {
                self.bump();
                members.push(TypeExpr {
                    kind: TypeExprKind::Null,
                    span: self.cur_span(),
                });
                self.bump();
            }
            ty = TypeExpr {
                kind: TypeExprKind::Union(members),
                span: self.span_from(lo),
            };
        }
        Ok(ty)
    }

    /// Class name after `instanceof` (generic arguments are speculative, as after `as`).
    pub(super) fn parse_instanceof_type(&mut self) -> PResult<TypeExpr> {
        if !self.at_ident_like() {
            self.error_expected("class name");
            return Err(Fail);
        }
        self.parse_named_type_with(true)
    }

    /// Primary type with `[]` suffixes (no top-level union).
    pub(super) fn parse_type_no_union(&mut self) -> PResult<TypeExpr> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            let prim = p.parse_type_prim()?;
            Ok(p.parse_array_suffixes(lo, prim))
        })
    }

    fn parse_array_suffixes(&mut self, lo: u32, mut ty: TypeExpr) -> TypeExpr {
        while self.at(Tok::LBracket) && self.nth(1) == Tok::RBracket {
            self.bump();
            self.bump();
            ty = TypeExpr {
                kind: TypeExprKind::Array(Box::new(ty)),
                span: self.span_from(lo),
            };
        }
        ty
    }

    fn parse_type_prim(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let span = self.cur_span();
        match self.peek() {
            Tok::Kw(Kw::Null) => {
                self.bump();
                Ok(TypeExpr {
                    kind: TypeExprKind::Null,
                    span,
                })
            }
            Tok::Kw(Kw::Void) => {
                self.bump();
                Ok(TypeExpr {
                    kind: TypeExprKind::Void,
                    span,
                })
            }
            Tok::LBracket => {
                self.bump();
                let elems = self.parse_type_seq(Tok::RBracket)?;
                self.expect(Tok::RBracket)?;
                Ok(TypeExpr {
                    kind: TypeExprKind::Tuple(elems),
                    span: self.span_from(lo),
                })
            }
            Tok::LParen => self.parse_fn_or_paren_type(),
            Tok::LBrace => self.parse_object_type(),
            _ if self.at_literal_type() => {
                let negative = self.eat(Tok::Minus);
                let lit = self.literal_at_cursor().expect("ICE: literal type token");
                self.bump();
                Ok(TypeExpr {
                    kind: TypeExprKind::Literal(SignedLit { lit, negative }),
                    span: self.span_from(lo),
                })
            }
            Tok::Ident if self.at_word("undefined") => Ok(self.undefined_type(span)),
            t if Self::is_ident_like(t) => self.parse_named_type(),
            _ => {
                self.error_expected("type");
                Err(Fail)
            }
        }
    }

    /// A string / number / `true` / `false` literal (numbers may have a leading `-`).
    fn at_literal_type(&self) -> bool {
        match self.peek() {
            Tok::Str(_) | Tok::Int(_) | Tok::Float(_) | Tok::Kw(Kw::True) | Tok::Kw(Kw::False) => {
                true
            }
            Tok::Minus => matches!(self.nth(1), Tok::Int(_) | Tok::Float(_)),
            _ => false,
        }
    }

    /// `{ name: T; other: U }` (`,` also separates fields; a trailing separator is allowed).
    fn parse_object_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(Tok::RBrace) {
            let flo = self.cur_lo();
            let name = self.parse_prop_name()?;
            let optional = self.eat(Tok::Question);
            self.expect(Tok::Colon)?;
            let mut ty = self.parse_type()?;
            if optional {
                ty = or_null(ty);
            }
            fields.push(ObjectTypeField {
                name,
                ty,
                optional,
                span: self.span_from(flo),
            });
            if !self.eat(Tok::Semi) && !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(TypeExpr {
            kind: TypeExprKind::Object(fields),
            span: self.span_from(lo),
        })
    }

    /// Comma-separated types up to (not including) `close`.
    fn parse_type_seq(&mut self, close: Tok) -> PResult<Vec<TypeExpr>> {
        let mut out = Vec::new();
        while !self.at(close) {
            out.push(self.parse_type()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    /// `(a: A) => R` when `=>` follows the matching `)`, else a parenthesized type `(A | B)`.
    /// Deciding by lookahead (not by trying the function type first) keeps nested parentheses
    /// linear: a failed attempt would re-parse everything inside once more per level.
    fn parse_fn_or_paren_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        if self.after_matching_paren(0) == Some(Tok::FatArrow) {
            return self.parse_fn_type();
        }
        self.bump(); // (
        let mut t = self.parse_type()?;
        self.expect(Tok::RParen)?;
        t.span = self.span_from(lo);
        Ok(t)
    }

    /// `(a: A, B) => R [throws E]` — parameter names are optional and dropped.
    fn parse_fn_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while !self.at(Tok::RParen) {
            if self.at_word("mut") && Self::is_ident_like(self.nth(1)) && self.nth(2) == Tok::Colon
            {
                self.reject_mut_modifier();
            }
            if self.at_ident_like() && self.nth(1) == Tok::Colon {
                self.bump();
                self.bump();
            }
            params.push(self.parse_type()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        self.expect(Tok::FatArrow)?;
        let ret = self.parse_type()?;
        let throws = self.parse_throws_clause()?.map(Box::new);
        Ok(TypeExpr {
            kind: TypeExprKind::Function {
                params,
                ret: Box::new(ret),
                throws,
            },
            span: self.span_from(lo),
        })
    }

    /// `a.b.C<Args>`
    pub(super) fn parse_named_type(&mut self) -> PResult<TypeExpr> {
        self.parse_named_type_with(false)
    }

    /// With `speculative_args`, a `<` that does not parse as type arguments is left alone.
    fn parse_named_type_with(&mut self, speculative_args: bool) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let mut path = vec![self.parse_ident()?];
        while self.at(Tok::Dot) && Self::is_name(self.nth(1)) {
            self.bump();
            path.push(self.take_ident());
        }
        let mut args = vec![];
        if self.at(Tok::Lt) {
            if speculative_args {
                args = self.speculate(|p| p.parse_type_args()).unwrap_or_default();
            } else {
                args = self.parse_type_args()?;
            }
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Named { path, args },
            span: self.span_from(lo),
        })
    }

    /// `<A, B>`
    pub(super) fn parse_type_args(&mut self) -> PResult<Vec<TypeExpr>> {
        self.expect(Tok::Lt)?;
        let args = self.parse_type_seq(Tok::Gt)?;
        self.expect(Tok::Gt)?;
        Ok(args)
    }
}

/// `T | null` for the written type of an optional parameter or field (`name?: T`), keeping
/// `T`'s span; a type that already admits `null` stays as it is.
pub(super) fn or_null(ty: TypeExpr) -> TypeExpr {
    let null = TypeExpr {
        kind: TypeExprKind::Null,
        span: Span::new(ty.span.file, ty.span.hi, ty.span.hi),
    };
    let span = ty.span;
    match ty.kind {
        TypeExprKind::Null => ty,
        TypeExprKind::Union(ms) if ms.iter().any(|m| matches!(m.kind, TypeExprKind::Null)) => {
            TypeExpr {
                kind: TypeExprKind::Union(ms),
                span,
            }
        }
        TypeExprKind::Union(mut ms) => {
            ms.push(null);
            TypeExpr {
                kind: TypeExprKind::Union(ms),
                span,
            }
        }
        _ => TypeExpr {
            kind: TypeExprKind::Union(vec![ty, null]),
            span,
        },
    }
}
