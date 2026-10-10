//! Type expressions: named types with generic arguments, `T[]`, tuples, function types, unions,
//! literal types (`"circle"`, `-1`, `true`), object types (`{ kind: "circle"; r: f64 }`), `null`
//! and `void`; `name?: T` (optional parameters and object-type fields) as `T | null`.

use super::{Fail, PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// Full type including unions: `A | B | null` (a leading `|` is allowed), whose members may
    /// be intersections (`A & B | C` is `(A & B) | C`).
    pub(super) fn parse_type(&mut self) -> PResult<TypeExpr> {
        self.guarded(|p| {
            let lo = p.cur_lo();
            p.eat(Tok::Pipe);
            let first = p.parse_intersection()?;
            if !p.at(Tok::Pipe) {
                return Ok(first);
            }
            let mut members = vec![first];
            while p.eat(Tok::Pipe) {
                members.push(p.parse_intersection()?);
            }
            Ok(TypeExpr {
                kind: TypeExprKind::Union(members),
                span: p.span_from(lo),
            })
        })
    }

    /// `A & B & …` (a leading `&` is allowed), or one type without a top-level union.
    fn parse_intersection(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        self.eat(Tok::Amp);
        let first = self.parse_type_no_union()?;
        if !self.at(Tok::Amp) {
            return Ok(first);
        }
        let mut members = vec![first];
        while self.eat(Tok::Amp) {
            members.push(self.parse_type_no_union()?);
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Intersection(members),
            span: self.span_from(lo),
        })
    }

    /// Type after `as`. Unions are limited to a trailing `| null` and string literals
    /// (`k as "a" | "b"`) so `x as u8 | y` stays a bit-or, and generic arguments are speculative
    /// so `a as i64 < b` stays a comparison.
    pub(super) fn parse_cast_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
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
        let mut ty = if self.at_ident_like() {
            let named = self.parse_named_type_with(true)?;
            self.parse_array_suffixes(lo, named)
        } else {
            self.parse_type_no_union()?
        };
        let strings = matches!(
            &ty.kind,
            TypeExprKind::Literal(SignedLit {
                lit: Lit::Str(_),
                ..
            })
        );
        let more = |p: &mut Self| {
            p.at(Tok::Pipe)
                && (p.nth(1) == Tok::Kw(Kw::Null) || strings && matches!(p.nth(1), Tok::Str(_)))
        };
        if more(self) {
            let mut members = vec![ty];
            while more(self) {
                self.bump();
                if self.at(Tok::Kw(Kw::Null)) {
                    members.push(TypeExpr {
                        kind: TypeExprKind::Null,
                        span: self.cur_span(),
                    });
                    self.bump();
                } else {
                    members.push(self.parse_type_no_union()?);
                }
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
            if p.at_readonly_type() {
                return p.parse_readonly_type();
            }
            let prim = p.parse_type_prim()?;
            let mut ty = p.parse_array_suffixes(lo, prim);
            // `T["k"]` / `T["a" | "b"]`: an indexed access (string keys only, so `x as T[0]`
            // and the like keep their meaning).
            while p.at(Tok::LBracket) && matches!(p.nth(1), Tok::Str(_)) {
                p.bump();
                let key = p.parse_type()?;
                p.expect(Tok::RBracket)?;
                let indexed = TypeExpr {
                    kind: TypeExprKind::Indexed {
                        object: Box::new(ty),
                        key: Box::new(key),
                    },
                    span: p.span_from(lo),
                };
                ty = p.parse_array_suffixes(lo, indexed);
            }
            Ok(ty)
        })
    }

    /// `readonly` as a type operator: before an array or tuple type (`readonly T[]`,
    /// `readonly [A, B]`), not a type named `readonly`.
    fn at_readonly_type(&mut self) -> bool {
        self.at(Tok::Kw(Kw::Readonly))
            && (Self::is_ident_like(self.nth(1))
                || matches!(self.nth(1), Tok::LBracket | Tok::LParen | Tok::LBrace)
                || self.nth(1) == Tok::Kw(Kw::Void)
                || self.nth(1) == Tok::Kw(Kw::Null))
    }

    /// `readonly T[]` (TypeScript's `ReadonlyArray<T>`, which it is parsed as) or
    /// `readonly [A, B]` (the tuple type: Velt tuples have no mutating methods). On any other
    /// type it is TypeScript's error TS1354.
    fn parse_readonly_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let kw = self.cur_span();
        self.bump(); // readonly
        let inner = self.parse_type_no_union()?;
        let span = self.span_from(lo);
        match inner.kind {
            TypeExprKind::Array(elem) => Ok(TypeExpr {
                kind: TypeExprKind::Named {
                    path: vec![Ident {
                        name: "ReadonlyArray".into(),
                        span: kw,
                    }],
                    args: vec![*elem],
                },
                span,
            }),
            TypeExprKind::Tuple(_) => Ok(TypeExpr {
                kind: inner.kind,
                span,
            }),
            _ => {
                self.error(
                    "'readonly' type modifier is only permitted on array and tuple literal types",
                    kw,
                );
                Ok(inner)
            }
        }
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
                let elems = self.parse_tuple_elems()?;
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
    fn at_literal_type(&mut self) -> bool {
        match self.peek() {
            Tok::Str(_) | Tok::Int(_) | Tok::Float(_) | Tok::Kw(Kw::True) | Tok::Kw(Kw::False) => {
                true
            }
            Tok::Minus => matches!(self.nth(1), Tok::Int(_) | Tok::Float(_)),
            _ => false,
        }
    }

    /// `{ name: T; readonly other?: U }` (`,` also separates fields; a trailing separator is
    /// allowed).
    fn parse_object_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(Tok::RBrace) {
            let flo = self.cur_lo();
            // `readonly` is a modifier only when a field name follows (a field may be named
            // `readonly`).
            let readonly = self.at(Tok::Kw(Kw::Readonly))
                && !matches!(
                    self.nth(1),
                    Tok::Colon | Tok::Question | Tok::Semi | Tok::Comma | Tok::RBrace
                );
            if readonly {
                self.bump();
            }
            let name = self.parse_prop_key()?;
            let optional = self.eat(Tok::Question);
            let mut ty = if self.at(Tok::LParen) || self.at(Tok::Lt) {
                self.parse_method_sig_type(flo, &name, optional)?
            } else {
                self.expect(Tok::Colon)?;
                self.parse_type()?
            };
            if optional {
                ty = or_null(ty);
            }
            fields.push(ObjectTypeField {
                name,
                ty,
                optional,
                readonly,
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

    /// Tuple element types up to (not including) `]`. Element labels (`[kind: string, b64:
    /// string]`) are documentation in TypeScript and are dropped.
    fn parse_tuple_elems(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut out = Vec::new();
        while !self.at(Tok::RBracket) {
            if Self::is_name(self.peek()) && self.nth(1) == Tok::Colon {
                self.bump();
                self.bump();
            }
            out.push(self.parse_type()?);
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        Ok(out)
    }

    /// A return type: a type, or a type predicate (`x is T`, `this is T`, `asserts x is T`,
    /// `asserts x`).
    pub(super) fn parse_ret_type(&mut self) -> PResult<TypeExpr> {
        let lo = self.cur_lo();
        let asserts = self.at_word("asserts")
            && (Self::is_ident_like(self.nth(1)) || self.nth(1) == Tok::Kw(Kw::This))
            && !matches!(self.nth(2), Tok::Lt | Tok::Dot | Tok::LBracket);
        let off = usize::from(asserts);
        let named = Self::is_ident_like(self.nth(off)) || self.nth(off) == Tok::Kw(Kw::This);
        if !(named && (asserts || self.nth_word(off + 1, "is"))) {
            return self.parse_type();
        }
        if asserts {
            self.bump();
        }
        let param = Box::new(self.take_ident());
        let ty = if self.at_word("is") {
            self.bump();
            Some(Box::new(self.parse_type()?))
        } else {
            None
        };
        Ok(TypeExpr {
            kind: TypeExprKind::Predicate { param, ty, asserts },
            span: self.span_from(lo),
        })
    }

    /// A method signature in an object type or an optional one in an interface (`m(x: T): R`,
    /// `m?(x: T): R`), after its name: a field of function type `(x: T) => R`. A generic one is
    /// not supported yet (function types have no type parameters).
    pub(super) fn parse_method_sig_type(
        &mut self,
        lo: u32,
        name: &Ident,
        optional: bool,
    ) -> PResult<TypeExpr> {
        let sig = self.parse_sig_rest(lo, name.clone(), false)?;
        if let Some(g) = sig.generics.first() {
            let msg = if optional {
                format!(
                    "an optional method signature can't be generic yet: `{}?` is a field of function type, and function types have no type parameters",
                    name.name
                )
            } else {
                format!(
                    "generic method signatures are only supported in interfaces: declare `{}` in an interface",
                    name.name
                )
            };
            self.error(msg, g.name.span);
        }
        let span = self.span_from(lo);
        let ret = sig.ret.unwrap_or(TypeExpr {
            kind: TypeExprKind::Void,
            span: Span::new(span.file, span.hi, span.hi),
        });
        Ok(TypeExpr {
            kind: TypeExprKind::Function {
                params: sig.params.into_iter().map(|p| p.ty).collect(),
                ret: Box::new(ret),
                throws: sig.throws.map(Box::new),
            },
            span,
        })
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
        let ret = self.parse_ret_type()?;
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
        // `unique symbol` (the type of a `const` holding a symbol) is `symbol`: which symbol a
        // constant holds is known from its initializer.
        if self.at_word("unique") && self.nth_word(1, "symbol") {
            self.bump();
        }
        let mut path = vec![self.parse_ident()?];
        while self.at(Tok::Dot) && Self::is_name(self.nth(1)) {
            self.bump();
            path.push(self.take_ident());
        }
        let mut args = vec![];
        if self.at(Tok::Lt) {
            let ts = path.len() == 1 && PROTOCOL_TYPES.contains(&path[0].name.as_str());
            if speculative_args {
                args = self
                    .speculate(|p| p.parse_type_args_with(ts))
                    .unwrap_or_default();
            } else {
                args = self.parse_type_args_with(ts)?;
            }
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Named { path, args },
            span: self.span_from(lo),
        })
    }

    /// `<A, B>`
    pub(super) fn parse_type_args(&mut self) -> PResult<Vec<TypeExpr>> {
        self.parse_type_args_with(false)
    }

    /// `<A, B>`; with `ts_return` (a protocol type's arguments), a later argument may be
    /// `undefined`.
    fn parse_type_args_with(&mut self, ts_return: bool) -> PResult<Vec<TypeExpr>> {
        self.expect(Tok::Lt)?;
        let mut args = Vec::new();
        while !self.at(Tok::Gt) {
            if ts_return && !args.is_empty() && self.at_ts_undefined_arg() {
                let name = self.take_ident();
                args.push(TypeExpr {
                    span: name.span,
                    kind: TypeExprKind::Named {
                        path: vec![name],
                        args: vec![],
                    },
                });
            } else {
                args.push(self.parse_type()?);
            }
            if !self.eat(Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::Gt)?;
        Ok(args)
    }

    /// `undefined` as a whole later type argument of a protocol type: TypeScript's `TReturn` /
    /// `TNext` (`Generator<number, undefined>`), which sema drops (velt_sema `ts_protocol`).
    fn at_ts_undefined_arg(&mut self) -> bool {
        self.at_word("undefined") && matches!(self.nth(1), Tok::Comma | Tok::Gt)
    }
}

/// The prelude's iteration protocol types, whose later type arguments may be TypeScript's
/// `TReturn` / `TNext` (velt_sema `ts_protocol`).
const PROTOCOL_TYPES: [&str; 10] = [
    "Generator",
    "AsyncGenerator",
    "Iterator",
    "AsyncIterator",
    "Iterable",
    "AsyncIterable",
    "IterableIterator",
    "AsyncIterableIterator",
    "IteratorObject",
    "IteratorResult",
];

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
