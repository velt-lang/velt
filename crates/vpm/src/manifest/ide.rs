//! Editor support for `package.vlt` (docs/internals/design/package-manifest.md "Editors"): where
//! the cursor is in the manifest object, and the completions and hover text the [`schema`] gives
//! there. Diagnostics are the reader's own ([`super::Manifest::read`]).
//!
//! The text is scanned with a small tokenizer instead of the parser, because completion runs
//! while the user types and the file rarely parses then (`{ name: "a", dep`).

use std::ops::Range;

use super::schema::{self, Field, Kind};

/// One completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    /// The field's type, or what the value is.
    pub detail: String,
    /// Markdown.
    pub doc: String,
    /// The bytes the completion replaces (the part already typed).
    pub replace: Range<u32>,
    /// What it inserts; LSP snippet syntax (`$1`) when `snippet` is set.
    pub text: String,
    pub snippet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Field,
    Value,
}

/// Hover text (Markdown) and the bytes it describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub markdown: String,
    pub range: Range<u32>,
}

/// The completions at byte `offset` of manifest text `src`.
pub fn completions(src: &str, offset: u32) -> Vec<Completion> {
    let scan = scan(src, offset);
    let Some(cursor) = scan.cursor else {
        return vec![];
    };
    let written = &scan.frames[cursor.frame];
    match cursor.at {
        At::Key => {
            let Some(fields) = schema::object_at(&written.path) else {
                return vec![];
            };
            let taken: Vec<&str> = written
                .keys
                .iter()
                .filter(|(_, lo)| Some(*lo) != cursor.token)
                .map(|(k, _)| k.as_str())
                .collect();
            fields
                .iter()
                .filter(|f| !taken.contains(&f.key))
                .map(|f| field_completion(f, &cursor))
                .collect()
        }
        At::Value(ref key) => {
            let Some(field) = schema::object_at(&written.path).and_then(|f| schema::field(f, key))
            else {
                return vec![];
            };
            match field.kind {
                Kind::Bool if !cursor.quoted => ["false", "true"]
                    .iter()
                    .map(|v| value_completion(v, v.to_string(), "bool", field.doc, &cursor))
                    .collect(),
                _ => vec![],
            }
        }
        At::Element => {
            let Some(Field {
                kind: Kind::StrArray { values },
                doc,
                ..
            }) = schema::field_at(&written.path)
            else {
                return vec![];
            };
            values
                .iter()
                .filter(|v| !written.strings.iter().any(|s| s == *v))
                .map(|v| {
                    let text = if cursor.quoted {
                        v.to_string()
                    } else {
                        format!("\"{v}\"")
                    };
                    value_completion(v, text, "string", doc, &cursor)
                })
                .collect()
        }
    }
}

/// Hover text for the key under byte `offset`, if the schema knows it.
pub fn hover(src: &str, offset: u32) -> Option<Hover> {
    let (path, key, range) = scan(src, offset).hovered?;
    let field = schema::field(schema::object_at(&path)?, &key)?;
    let optional = if field.required { "" } else { "?" };
    Some(Hover {
        markdown: format!(
            "```velt\n{}{optional}: {}\n```\n{}",
            field.key, field.ty, field.doc
        ),
        range,
    })
}

fn field_completion(f: &Field, cursor: &Cursor) -> Completion {
    let (text, snippet) = if cursor.quoted {
        (f.key.to_string(), false)
    } else {
        let value = match f.kind {
            Kind::Str => "\"$1\"",
            Kind::Bool => "${1|false,true|}",
            Kind::StrArray { .. } => "[$1]",
            Kind::Object(_) | Kind::Map(_) => "{ $1 }",
        };
        (format!("{}: {value}", f.key), true)
    };
    Completion {
        label: f.key.to_string(),
        kind: CompletionKind::Field,
        detail: f.ty.to_string(),
        doc: f.doc.to_string(),
        replace: cursor.replace.clone(),
        text,
        snippet,
    }
}

fn value_completion(label: &str, text: String, ty: &str, doc: &str, c: &Cursor) -> Completion {
    Completion {
        label: label.to_string(),
        kind: CompletionKind::Value,
        detail: ty.to_string(),
        doc: doc.to_string(),
        replace: c.replace.clone(),
        text,
        snippet: false,
    }
}

// ── Scanning ──────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Eq,
    /// A string literal (its decoded text); `closed` unless it runs to the end of the line.
    Str {
        text: String,
        closed: bool,
    },
    Ident(String),
    /// A number, template, or any other character.
    Other,
}

struct Token {
    tok: Tok,
    lo: u32,
    hi: u32,
}

fn tokenize(src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let lo = i;
        let tok = match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                continue;
            }
            b'"' | b'\'' | b'`' => {
                i += 1;
                let mut text = String::new();
                let mut closed = false;
                while i < b.len() {
                    match b[i] {
                        q if q == c => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        b'\n' if c != b'`' => break,
                        b'\\' if i + 1 < b.len() => {
                            text.push(b[i + 1] as char);
                            i += 2;
                        }
                        _ => {
                            let ch = src[i..].chars().next().unwrap_or('\0');
                            text.push(ch);
                            i += ch.len_utf8().max(1);
                        }
                    }
                }
                if c == b'`' {
                    Tok::Other
                } else {
                    Tok::Str { text, closed }
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$')
                {
                    i += 1;
                }
                Tok::Ident(src[lo..i].to_string())
            }
            _ => {
                i += src[i..].chars().next().map_or(1, char::len_utf8);
                match c {
                    b'{' => Tok::LBrace,
                    b'}' => Tok::RBrace,
                    b'[' => Tok::LBracket,
                    b']' => Tok::RBracket,
                    b':' => Tok::Colon,
                    b',' => Tok::Comma,
                    b'=' => Tok::Eq,
                    _ => Tok::Other,
                }
            }
        };
        out.push(Token {
            tok,
            lo: lo as u32,
            hi: i as u32,
        });
    }
    out
}

/// An object or array of the manifest value.
struct Frame {
    object: bool,
    /// Keys from the manifest object down to this container (`"*"` for an array element).
    path: Vec<String>,
    /// For objects: the keys written, with their start offsets.
    keys: Vec<(String, u32)>,
    /// For arrays: the string elements written.
    strings: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Key,
    AfterKey,
    Value,
    AfterValue,
}

#[derive(Clone, Debug, PartialEq)]
enum At {
    Key,
    Value(String),
    Element,
}

struct Cursor {
    frame: usize,
    at: At,
    replace: Range<u32>,
    /// The cursor is inside a string literal (`replace` is its contents).
    quoted: bool,
    /// The start of the token the cursor is in (none between tokens).
    token: Option<u32>,
}

struct Scan {
    frames: Vec<Frame>,
    cursor: Option<Cursor>,
    hovered: Option<(Vec<String>, String, Range<u32>)>,
}

/// Walk the manifest object of `src`, noting where `offset` is.
fn scan(src: &str, offset: u32) -> Scan {
    let tokens = tokenize(src);
    let mut scan = Scan {
        frames: vec![],
        cursor: None,
        hovered: None,
    };
    // Before the manifest value: `import type { … } from "velt:package"; export const pkg … =`.
    let Some(start) = tokens.iter().position(|t| t.tok == Tok::Eq) else {
        return scan;
    };
    // (frame, state, pending key) for each open container.
    let mut stack: Vec<(usize, State, Option<String>)> = vec![];
    for (i, t) in tokens.iter().enumerate().skip(start + 1) {
        if stack.is_empty() {
            if t.tok != Tok::LBrace || !scan.frames.is_empty() {
                if scan.frames.is_empty() {
                    continue; // e.g. whitespace-free junk before `{`
                }
                break; // after the manifest object
            }
            if offset <= t.lo {
                return scan; // the cursor is before the manifest object
            }
            scan.frames.push(Frame {
                object: true,
                path: vec![],
                keys: vec![],
                strings: vec![],
            });
            stack.push((0, State::Key, None));
            continue;
        }
        if scan.cursor.is_none() {
            note_cursor(&mut scan, &stack, t, offset);
        }
        let (frame, state, pending) = stack.last().cloned().expect("ICE: a container is open");
        let object = scan.frames[frame].object;
        let top = stack.len() - 1;
        let set = |stack: &mut Vec<(usize, State, Option<String>)>, s: State| stack[top].1 = s;
        match (&t.tok, state) {
            (Tok::RBrace | Tok::RBracket, _) => {
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.1 = State::AfterValue;
                }
            }
            (Tok::Ident(k) | Tok::Str { text: k, .. }, State::Key) if object => {
                if t.lo <= offset && offset <= t.hi {
                    let path = scan.frames[frame].path.clone();
                    scan.hovered = Some((path, k.clone(), t.lo..t.hi));
                }
                scan.frames[frame].keys.push((k.clone(), t.lo));
                stack[top].2 = Some(k.clone());
                set(&mut stack, State::AfterKey);
            }
            (Tok::Colon, State::AfterKey) => set(&mut stack, State::Value),
            (Tok::Comma, _) => set(&mut stack, if object { State::Key } else { State::Value }),
            (Tok::LBrace | Tok::LBracket, State::Value) => {
                let mut path = scan.frames[frame].path.clone();
                path.push(if object {
                    pending.unwrap_or_default()
                } else {
                    "*".into()
                });
                scan.frames.push(Frame {
                    object: t.tok == Tok::LBrace,
                    path,
                    keys: vec![],
                    strings: vec![],
                });
                let state = if t.tok == Tok::LBrace {
                    State::Key
                } else {
                    State::Value
                };
                stack.push((scan.frames.len() - 1, state, None));
            }
            (tok, State::Value) => {
                if let (false, Tok::Str { text, .. }) = (object, tok) {
                    scan.frames[frame].strings.push(text.clone());
                }
                set(&mut stack, State::AfterValue);
            }
            _ => {}
        }
        if i + 1 == tokens.len() && scan.cursor.is_none() && offset >= t.hi {
            if let Some(&(frame, state, ref pending)) = stack.last() {
                gap_cursor(&mut scan, frame, state, pending, offset);
            }
        }
    }
    scan
}

/// If `offset` is in the gap before `t`, or inside `t` while it is being typed, record where.
fn note_cursor(scan: &mut Scan, stack: &[(usize, State, Option<String>)], t: &Token, offset: u32) {
    let &(frame, state, ref pending) = stack.last().expect("ICE: a container is open");
    if offset <= t.lo {
        gap_cursor(scan, frame, state, pending, offset);
        return;
    }
    let (replace, quoted) = match &t.tok {
        Tok::Ident(_) if offset <= t.hi => (t.lo..t.hi, false),
        Tok::Str { closed, .. } if offset < t.hi || !closed => {
            let end = if *closed { t.hi - 1 } else { t.hi };
            (t.lo + 1..end, true)
        }
        _ => return,
    };
    let at = match (scan.frames[frame].object, state) {
        (true, State::Key) => At::Key,
        (true, State::Value) => At::Value(pending.clone().unwrap_or_default()),
        (false, State::Value) => At::Element,
        _ => return,
    };
    scan.cursor = Some(Cursor {
        frame,
        at,
        replace,
        quoted,
        token: Some(t.lo),
    });
}

fn gap_cursor(scan: &mut Scan, frame: usize, state: State, pending: &Option<String>, offset: u32) {
    let at = match (scan.frames[frame].object, state) {
        (true, State::Key) => At::Key,
        (true, State::Value) => At::Value(pending.clone().unwrap_or_default()),
        (false, State::Value) => At::Element,
        _ => return,
    };
    scan.cursor = Some(Cursor {
        frame,
        at,
        replace: offset..offset,
        quoted: false,
        token: None,
    });
}

#[cfg(test)]
mod tests;
