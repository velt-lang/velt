//! Signature help: the parameter list of the call whose parentheses enclose the cursor, with the
//! parameter being typed highlighted. The call is found in the text (while typing, the call usually
//! does not parse yet): the innermost unclosed `(` that follows a callee name, and the commas
//! before the cursor at its level. The callee is resolved through sema. Its doc comment documents
//! the signature (the description, return value and exceptions) and each parameter (`@param`).

use lsp_types::{
    Documentation, MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, SignatureHelp,
    SignatureInformation,
};
use velt_doc::comment::DocComment;
use velt_sema::ide::DefRef;

use crate::analysis::Analysis;
use crate::text_scan::{self, Token, TokenKind};
use crate::{callable, completion, docs};

/// Signature help at byte `offset` of the document.
pub fn signature_help(analysis: &Analysis, offset: u32) -> Option<SignatureHelp> {
    let text = analysis.text();
    let tokens = text_scan::scan(text, offset as usize);
    let (callee, is_new, commas) =
        open_calls(&tokens)
            .into_iter()
            .rev()
            .find_map(|(paren, commas)| {
                callee_before(text, &tokens, paren).map(|(c, n)| (c, n, commas))
            })?;
    let def = resolve(analysis, text, &tokens, callee)?;
    let sig = callable::signature_of(analysis, &def, is_new)?;
    let active = commas.min(sig.params.len().saturating_sub(1)) as u32;
    let utf16 = |i: usize| sig.label[..i].encode_utf16().count() as u32;
    let doc = doc_of(analysis, &def, is_new).unwrap_or_default();
    let parameters = sig
        .params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::LabelOffsets([utf16(p.lo), utf16(p.hi)]),
            documentation: doc.param(&p.name).filter(|t| !t.is_empty()).map(markdown),
        })
        .collect();
    // The parameters have their own documentation, and examples are too long here.
    let summary = DocComment {
        params: vec![],
        examples: vec![],
        see: vec![],
        ..doc
    }
    .render_markdown();
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: sig.label.clone(),
            documentation: (!summary.is_empty()).then(|| markdown(&summary)),
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// The doc comment of the called definition: for `new C(` the constructor's, else the class's.
fn doc_of(analysis: &Analysis, def: &DefRef, is_new: bool) -> Option<DocComment> {
    if is_new && def.kind.is_type() {
        let ctor = callable::constructor_of(analysis, def);
        if let Some(doc) = ctor.and_then(|c| docs::doc_for(analysis, &c)) {
            return Some(doc);
        }
    }
    docs::doc_for(analysis, def)
}

fn markdown(text: &str) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value: text.to_string(),
    })
}

/// Unclosed `(` tokens (outermost first) with the number of commas at their level so far.
fn open_calls(tokens: &[Token]) -> Vec<(usize, usize)> {
    // Open brackets: (token index, bracket, commas at its level).
    let mut stack: Vec<(usize, u8, usize)> = vec![];
    for (i, t) in tokens.iter().enumerate() {
        let TokenKind::Punct(c) = t.kind else {
            continue;
        };
        match c {
            b'(' | b'[' | b'{' => stack.push((i, c, 0)),
            b')' | b']' | b'}' => {
                let open = match c {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if let Some(pos) = stack.iter().rposition(|(_, b, _)| *b == open) {
                    stack.truncate(pos);
                }
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.2 += 1;
                }
            }
            // A `;` outside parentheses ends a statement: calls before it are complete.
            b';' if !stack.iter().any(|(_, b, _)| *b == b'(') => stack.clear(),
            _ => {}
        }
    }
    stack
        .into_iter()
        .filter(|(_, b, _)| *b == b'(')
        .map(|(i, _, commas)| (i, commas))
        .collect()
}

/// The callee identifier token before the `(` at `paren` (skipping explicit type arguments), and
/// whether it follows `new`. `None` for declarations and `if (` / `while (` heads.
fn callee_before(text: &str, tokens: &[Token], paren: usize) -> Option<(usize, bool)> {
    let mut i = paren.checked_sub(1)?;
    if tokens[i].kind == TokenKind::Punct(b'>') {
        i = matching_angle(tokens, i)?.checked_sub(1)?;
    }
    if tokens[i].kind != TokenKind::Ident {
        return None;
    }
    let word = |t: &Token| &text[t.lo as usize..t.hi as usize];
    if completion::is_keyword(word(&tokens[i])) {
        return None;
    }
    let before = i.checked_sub(1).map(|j| &tokens[j]);
    if before.is_some_and(|t| t.kind == TokenKind::Ident && word(t) == "function") {
        return None;
    }
    let is_new = before.is_some_and(|t| t.kind == TokenKind::Ident && word(t) == "new");
    Some((i, is_new))
}

/// Index of the `<` matching the `>` at `close`.
fn matching_angle(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0;
    for i in (0..=close).rev() {
        match tokens[i].kind {
            TokenKind::Punct(b'>') => depth += 1,
            TokenKind::Punct(b'<') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            TokenKind::Punct(b'(' | b')' | b';' | b'{' | b'}') => return None,
            _ => {}
        }
    }
    None
}

/// What the callee token names: sema's answer at the name, else a lookup of the name among the
/// names visible there (or the members of the receiver before a `.`).
fn resolve(analysis: &Analysis, text: &str, tokens: &[Token], callee: usize) -> Option<DefRef> {
    let t = tokens[callee];
    if let Some(def) = crate::sema_query::def_at(analysis, (t.lo + t.hi) / 2) {
        return Some(def);
    }
    let ide = analysis.ide.as_ref()?;
    let name = &text[t.lo as usize..t.hi as usize];
    let scope = ide.scope_at(analysis.file(), t.lo);
    let is_member = callee >= 2 && tokens[callee - 1].kind == TokenKind::Punct(b'.');
    if !is_member {
        return scope.into_iter().find(|(n, _)| n == name).map(|(_, d)| d);
    }
    let r = tokens[callee - 2];
    let receiver = &text[r.lo as usize..r.hi as usize];
    let (_, receiver_def) = scope.into_iter().find(|(n, _)| n == receiver)?;
    ide.members_of(&receiver_def)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, d, _)| d)
}
