//! Members of interface and object types that need more than a field parser: call and
//! construct signatures and the end of a member (`;`, `,` or a line break).

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{Kw, Tok};
use velt_common::Span;

impl<'a> Parser<'a> {
    /// At a call signature (`(x: T): R`, `<T>(x: T): T`) or a construct signature
    /// (`new (x: T): R`) in an interface or object type: parsed into the function type it
    /// declares (`(x: T) => R`, `<T>(x: T) => T`, `new (x: T) => R`), else `None`. (In
    /// TypeScript `new(…)` there is always a construct signature; a method named `new` is
    /// written `"new"(…)`. In a class, `new()` is a method.)
    pub(super) fn parse_call_signature(&mut self) -> PResult<Option<TypeExpr>> {
        let construct = self.at(Tok::Kw(Kw::New)) && matches!(self.nth(1), Tok::LParen | Tok::Lt);
        if !(construct || self.at(Tok::LParen) || self.at(Tok::Lt)) {
            return Ok(None);
        }
        let lo = self.cur_lo();
        if construct {
            self.bump(); // new
        }
        let name = Ident {
            name: String::new(),
            span: self.cur_span(),
        };
        let sig = self.parse_sig_rest(lo, name, false)?;
        let span = self.span_from(lo);
        let mut ty = sig_type(sig, span);
        if construct {
            ty = TypeExpr {
                kind: TypeExprKind::Construct(Box::new(ty)),
                span,
            };
        }
        Ok(Some(ty))
    }

    /// An interface with call or construct signatures `sigs` (and possibly an index signature):
    /// the type alias of the type it spells ([`Self::callable_type`]), its methods as fields of
    /// function type and its bases as parts of the intersection.
    pub(super) fn callable_interface(
        &mut self,
        lo: u32,
        decl: InterfaceDecl,
        index: Option<super::types::IndexSignature>,
        sigs: Vec<TypeExpr>,
    ) -> ItemKind {
        let span = self.span_from(lo);
        let mut fields: Vec<ObjectTypeField> = decl
            .fields
            .into_iter()
            .map(|f| ObjectTypeField {
                name: f.name,
                ty: match f.optional {
                    true => super::types::or_null(f.ty),
                    false => f.ty,
                },
                optional: f.optional,
                readonly: f.readonly,
                span: f.span,
            })
            .collect();
        for m in decl.methods {
            if m.is_getter || m.is_setter || m.body.is_some() {
                self.error(
                    "an interface with call signatures can't have accessors or methods with bodies",
                    m.sig.name.span,
                );
                continue;
            }
            let name = m.sig.name.clone();
            let field_span = m.sig.span;
            fields.push(ObjectTypeField {
                name,
                ty: sig_type(m.sig, field_span),
                optional: false,
                readonly: false,
                span: field_span,
            });
        }
        let mut rest = decl.extends;
        if !fields.is_empty() || index.is_some() {
            let kind = match index {
                Some(ix) => ix.into_type(fields),
                None => TypeExprKind::Object(fields),
            };
            rest.push(TypeExpr { kind, span });
        }
        ItemKind::TypeAlias(TypeAlias {
            name: decl.name,
            generics: decl.generics,
            ty: Self::callable_type(sigs, rest, span),
        })
    }

    /// The type an object type or interface body with call or construct signatures `sigs`
    /// spells, as in TypeScript: one signature alone is that function type; with more
    /// signatures (overloads) or with members (`rest`: the object type of the members, the
    /// bases of an interface), the intersection of all of them.
    pub(super) fn callable_type(sigs: Vec<TypeExpr>, rest: Vec<TypeExpr>, span: Span) -> TypeExpr {
        // Errors about the whole type point at its first signature.
        let span = sigs.first().map_or(span, |s| s.span);
        let mut parts = sigs;
        parts.extend(rest);
        if parts.len() == 1 {
            return parts.pop().expect("ICE: one part");
        }
        TypeExpr {
            kind: TypeExprKind::Intersection(parts),
            span,
        }
    }

    /// The end of an object type's member: `;` or `,` (consumed), or a line break before the
    /// next member (TypeScript's automatic semicolon insertion).
    pub(super) fn eat_type_member_end(&mut self) -> bool {
        self.eat(Tok::Semi) || self.eat(Tok::Comma) || self.line_break_before()
    }
}

/// The function type a signature declares: `(x: T) => R`, `<T>(x: T) => R` (a missing return
/// type is `void`).
fn sig_type(sig: FnSig, span: Span) -> TypeExpr {
    let ret = sig.ret.unwrap_or(TypeExpr {
        kind: TypeExprKind::Void,
        span: Span::new(span.file, span.hi, span.hi),
    });
    let ty = TypeExpr {
        kind: TypeExprKind::Function {
            rest: sig.params.last().is_some_and(|p| p.rest),
            params: sig.params.into_iter().map(|p| p.ty).collect(),
            ret: Box::new(ret),
            throws: sig.throws.map(Box::new),
        },
        span,
    };
    match sig.generics.is_empty() {
        true => ty,
        false => TypeExpr {
            kind: TypeExprKind::GenericFunction {
                type_params: sig.generics,
                ty: Box::new(ty),
            },
            span,
        },
    }
}
