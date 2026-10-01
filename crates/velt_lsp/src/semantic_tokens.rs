//! Semantic tokens: every identifier sema resolved, colored by what it denotes (type, function,
//! method, parameter, property, local...) with modifiers for declarations, `const`/`readonly`
//! bindings, `static` members, `let` bindings (`mutable`) and standard-library definitions.
//! Keywords, literals and comments are left to the TextMate grammar.

use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};
use velt_common::Span;
use velt_sema::ide::{DefKind, DefRef};

use crate::analysis::Analysis;
use crate::completion;
use crate::line_index::LineIndex;
use crate::text_scan::{self, TokenKind};

/// Token types, in legend order (a token's type is its index here).
const TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::CLASS,
    SemanticTokenType::STRUCT,
    SemanticTokenType::INTERFACE,
    SemanticTokenType::ENUM,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::TYPE,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::METHOD,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
];

/// Modifier bits, in legend order.
const DECLARATION: u32 = 1 << 0;
const READONLY: u32 = 1 << 1;
const STATIC: u32 = 1 << 2;
const DEFAULT_LIBRARY: u32 = 1 << 3;
const MUTABLE: u32 = 1 << 4;

/// The legend announced in the server capabilities.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TYPES.to_vec(),
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::STATIC,
            SemanticTokenModifier::DEFAULT_LIBRARY,
            SemanticTokenModifier::new("mutable"),
        ],
    }
}

/// Tokens of the whole document.
pub fn semantic_tokens(analysis: &Analysis) -> SemanticTokens {
    let text = analysis.text();
    let index = LineIndex::new(text);
    let mut data = vec![];
    let (mut line, mut col) = (0, 0);
    for t in text_scan::scan(text, text.len()) {
        if t.kind != TokenKind::Ident || completion::is_keyword(&text[t.lo as usize..t.hi as usize])
        {
            continue;
        }
        let Some(def) = crate::sema_query::def_at(analysis, (t.lo + t.hi) / 2) else {
            continue;
        };
        let span = Span::new(analysis.file(), t.lo, t.hi);
        let (ty, modifiers) = classify(analysis, &def, span);
        let start = index.position(t.lo);
        let length = text[t.lo as usize..t.hi as usize].encode_utf16().count() as u32;
        let delta_line = start.line - line;
        let delta_start = if delta_line == 0 {
            start.character - col
        } else {
            start.character
        };
        (line, col) = (start.line, start.character);
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: ty,
            token_modifiers_bitset: modifiers,
        });
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

/// Token type index and modifier bits of an identifier at `span` naming `def`.
fn classify(analysis: &Analysis, def: &DefRef, span: Span) -> (u32, u32) {
    let ty = match def.kind {
        DefKind::Class | DefKind::Constructor => SemanticTokenType::CLASS,
        DefKind::Struct => SemanticTokenType::STRUCT,
        DefKind::Interface => SemanticTokenType::INTERFACE,
        DefKind::Enum => SemanticTokenType::ENUM,
        DefKind::Variant => SemanticTokenType::ENUM_MEMBER,
        DefKind::TypeAlias => SemanticTokenType::TYPE,
        DefKind::Function | DefKind::ExternFunction => SemanticTokenType::FUNCTION,
        DefKind::Method | DefKind::StaticMethod => SemanticTokenType::METHOD,
        DefKind::Field | DefKind::Getter | DefKind::StaticField => SemanticTokenType::PROPERTY,
        DefKind::Constant | DefKind::Local => SemanticTokenType::VARIABLE,
        DefKind::Parameter => SemanticTokenType::PARAMETER,
    };
    let index = TYPES.iter().position(|t| *t == ty).unwrap_or(0) as u32;
    let mut modifiers = 0;
    if def.span == span {
        modifiers |= DECLARATION;
    }
    let readonly = def.kind == DefKind::Constant
        || def.detail.starts_with("const ")
        || def.detail.contains("readonly ");
    if readonly {
        modifiers |= READONLY;
    }
    if def.detail.starts_with("let ") {
        modifiers |= MUTABLE;
    }
    if matches!(def.kind, DefKind::StaticMethod | DefKind::StaticField) {
        modifiers |= STATIC;
    }
    if def.span == Span::DUMMY || analysis.is_std(def.module) {
        modifiers |= DEFAULT_LIBRARY;
    }
    (index, modifiers)
}
