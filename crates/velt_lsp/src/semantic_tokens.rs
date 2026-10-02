//! Semantic tokens: every identifier sema resolved, colored by what it denotes (type, function,
//! method, parameter, property, local...) with modifiers for declarations, `const`/`readonly`
//! bindings, `static` members, `let` bindings (`mutable`), standard-library definitions, and
//! calls of functions that modify their receiver or arguments (`mutating`, as inferred).
//! Keywords, literals and comments are left to the TextMate grammar.
//!
//! Besides the whole document, a range of it can be asked for, and a [`delta`] against the
//! tokens sent last (the server keeps them per document with a result id).

use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensEdit,
    SemanticTokensLegend,
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
const MUTATING: u32 = 1 << 5;

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
            SemanticTokenModifier::new("mutating"),
        ],
    }
}

/// Tokens of the whole document.
pub fn semantic_tokens(analysis: &Analysis) -> Vec<SemanticToken> {
    let len = analysis.text().len() as u32;
    tokens_in(analysis, 0, len)
}

/// Tokens starting in the document's byte range `lo..hi` (relative to the first of them, as
/// `textDocument/semanticTokens/range` answers).
pub fn tokens_in(analysis: &Analysis, lo: u32, hi: u32) -> Vec<SemanticToken> {
    let text = analysis.text();
    let index = LineIndex::new(text);
    let mut data = vec![];
    let (mut line, mut col) = (0, 0);
    // Scan past `hi` to the end of the identifier there, so the last one is not cut short.
    let hi_usize = (hi as usize).min(text.len());
    let end = hi_usize
        + text
            .get(hi_usize..)
            .unwrap_or("")
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            .count();
    let scanned = text_scan::scan(text, end);
    for t in scanned.into_iter().filter(|t| t.lo >= lo && t.lo < hi) {
        if t.kind != TokenKind::Ident || completion::is_keyword(&text[t.lo as usize..t.hi as usize])
        {
            continue;
        }
        let Some(def) = crate::sema_query::def_at(analysis, (t.lo + t.hi) / 2) else {
            continue;
        };
        let span = Span::new(analysis.file(), t.lo, t.hi);
        let (ty, mut modifiers) = classify(analysis, &def, span);
        if def.span != span && is_call(text, t.hi) && mutates(analysis, &def) {
            modifiers |= MUTATING;
        }
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
    data
}

/// The one edit turning `old` into `new` (`textDocument/semanticTokens/full/delta`): the changed
/// middle between their common prefix and suffix, in units of the flat integer array (five per
/// token). Empty when nothing changed.
pub fn delta(old: &[SemanticToken], new: &[SemanticToken]) -> Vec<SemanticTokensEdit> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old.len() && old.len() == new.len() {
        return vec![];
    }
    vec![SemanticTokensEdit {
        start: (prefix * 5) as u32,
        delete_count: ((old.len() - prefix - suffix) * 5) as u32,
        data: Some(new[prefix..new.len() - suffix].to_vec()),
    }]
}

/// Is the identifier ending at `hi` called (`f(`, `x.add (`)?
fn is_call(text: &str, hi: u32) -> bool {
    text.get(hi as usize..)
        .is_some_and(|rest| rest.trim_start().starts_with('('))
}

/// Does calling `def` modify its receiver or an argument (as inferred)?
fn mutates(analysis: &Analysis, def: &DefRef) -> bool {
    analysis
        .ide
        .as_ref()
        .and_then(|ide| ide.mutation_of(def))
        .is_some_and(|m| m.any())
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

#[cfg(test)]
mod tests {
    use lsp_types::SemanticToken;

    use super::delta;

    fn tok(n: u32) -> SemanticToken {
        SemanticToken {
            delta_line: n,
            delta_start: 0,
            length: 1,
            token_type: 0,
            token_modifiers_bitset: 0,
        }
    }

    #[test]
    fn delta_replaces_the_changed_middle() {
        let old = [tok(1), tok(2), tok(3), tok(4)];
        let new = [tok(1), tok(9), tok(9), tok(4)];
        let edits = delta(&old, &new);
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].start, edits[0].delete_count), (5, 10));
        assert_eq!(edits[0].data.as_deref(), Some(&new[1..3]));
        assert!(delta(&old, &old).is_empty());
        let grown = delta(&old[..2], &old);
        assert_eq!((grown[0].start, grown[0].delete_count), (10, 0));
        let shrunk = delta(&[tok(1), tok(1)], &[tok(1)]);
        assert_eq!((shrunk[0].start, shrunk[0].delete_count), (5, 5));
    }
}
