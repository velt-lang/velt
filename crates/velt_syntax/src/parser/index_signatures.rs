//! Index signatures of interfaces and object types: `[key: K]: V`, and a template-literal key
//! as a member of its own ([`crate::ast::key_pattern_name`]).

use velt_common::Span;

use super::{PResult, Parser};
use crate::ast::{Ident, IndexKey, ObjectTypeField, TypeExpr, TypeExprKind};
use crate::lexer::Tok;

impl Parser<'_> {
    /// At `[name: K]` (an index signature, after any `readonly`)?
    pub(super) fn at_index_signature(&mut self) -> bool {
        self.at(Tok::LBracket) && Self::is_name(self.nth(1)) && self.nth(2) == Tok::Colon
    }

    /// `[key: K]: V` (the cursor at `[`).
    pub(super) fn parse_index_signature(
        &mut self,
        lo: u32,
        readonly: bool,
    ) -> PResult<IndexSignature> {
        self.bump(); // [
        let key_name = self.take_ident();
        self.bump(); // :
        let key = self.parse_type()?;
        self.expect(Tok::RBracket)?;
        if self.at(Tok::Question) {
            let span = self.cur_span();
            self.error("an index signature cannot be optional", span);
            self.bump();
        }
        self.expect(Tok::Colon)?;
        let value = self.parse_type()?;
        Ok(IndexSignature {
            key_name,
            key,
            value,
            readonly,
            span: self.span_from(lo),
        })
    }

    /// Records index signature `sig` of an object type or interface; a second one (`[k:
    /// number]` next to `[k: string]`) is not supported yet.
    pub(super) fn add_index_signature(
        &mut self,
        index: &mut Option<IndexSignature>,
        sig: IndexSignature,
    ) {
        if index.is_some() {
            self.error(
                "more than one index signature in a type is not supported yet (#937)",
                sig.span,
            );
            return;
        }
        *index = Some(sig);
    }
}

/// An index signature while its object type or interface is parsed.
pub(super) struct IndexSignature {
    pub key_name: Ident,
    pub key: TypeExpr,
    pub value: TypeExpr,
    pub readonly: bool,
    pub span: Span,
}

impl IndexSignature {
    /// The member name of a template-literal key with only `${string}` holes
    /// ([`crate::ast::key_pattern_name`]).
    pub(super) fn pattern_name(&self) -> Option<Ident> {
        let TypeExprKind::TemplateLiteral { quasis, types } = &self.key.kind else {
            return None;
        };
        let strings = types.iter().all(|t| {
            matches!(&t.kind, TypeExprKind::Named { path, args }
                if args.is_empty() && matches!(path.as_slice(), [n] if n.name == "string"))
        });
        strings.then(|| Ident {
            name: crate::ast::key_pattern_name(&self.key_name.name, quasis),
            span: self.key.span,
        })
    }

    /// The object type with this index signature and the named members `fields`.
    pub(super) fn into_type(self, fields: Vec<ObjectTypeField>) -> TypeExprKind {
        TypeExprKind::IndexSig {
            fields,
            key: Box::new(IndexKey {
                name: self.key_name,
                ty: self.key,
            }),
            value: Box::new(self.value),
            readonly: self.readonly,
        }
    }
}
