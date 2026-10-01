//! JSX lexing. In code a `<` is always `Lt`; the parser decides where an element starts (only
//! where it expects an expression, and not before a generic arrow `<T>(x: T) => x`) and re-lexes
//! from there ([`Lexer::relex_jsx`]). Inside an element, child elements and element attribute
//! values start with `JsxLt` directly.
//!
//! Inside a tag (`Mode::JsxTag`) names may contain `-`, strings have no backslash escapes and
//! `{` opens an expression container. Between the tags (`Mode::JsxChildren`) everything up to
//! the next `{` or `<` is one text token, cooked with React's whitespace rules (see
//! [`clean_text`]) and HTML entities decoded.

use super::entities::decode_entities;
use super::{is_ident_continue, is_ident_start, Lexer, Mode, Payload, Tok};

impl Lexer<'_> {
    /// One token inside a tag (trivia already skipped). `None` = skipped a bad character.
    pub(super) fn jsx_tag_token(&mut self) -> Option<Tok> {
        let start = self.pos;
        let c = self.at(0);
        let tok = match c {
            _ if is_ident_start(c) => {
                while is_ident_continue(self.at(0)) || self.at(0) == b'-' {
                    self.pos += 1;
                }
                Tok::JsxIdent
            }
            b'"' | b'\'' => self.jsx_string(c),
            b'{' => {
                self.pos += 1;
                self.push_mode(Mode::JsxExpr);
                Tok::LBrace
            }
            // An element as an attribute value: `<A icon=<Star /> />`.
            b'<' => {
                self.pos += 1;
                self.push_mode(Mode::JsxTag { closing: false });
                Tok::JsxLt
            }
            b'/' if self.at(1) == b'>' => {
                self.pos += 2;
                self.end_tag(true);
                Tok::JsxSlashGt
            }
            b'>' => {
                self.pos += 1;
                self.end_tag(false);
                Tok::JsxGt
            }
            b':' | b'.' | b'=' => {
                self.pos += 1;
                match c {
                    b':' => Tok::Colon,
                    b'.' => Tok::Dot,
                    _ => Tok::Eq,
                }
            }
            _ => {
                self.unexpected_char(start);
                return None;
            }
        };
        Some(tok)
    }

    /// Leaves a tag at its `>` / `/>`: an opening tag's `>` starts its children; a closing tag
    /// (or a self-closing one) ends the element, and a closing tag also ends the children.
    fn end_tag(&mut self, self_closing: bool) {
        match self.mode() {
            Some(Mode::JsxTag { closing: true }) => {
                self.pop_mode();
                if self.mode() == Some(Mode::JsxChildren) {
                    self.pop_mode();
                }
            }
            Some(Mode::JsxTag { closing: false }) => {
                self.pop_mode();
                if !self_closing {
                    self.push_mode(Mode::JsxChildren);
                }
            }
            _ => {}
        }
    }

    /// An attribute string: no escapes, may span lines, entities decoded.
    fn jsx_string(&mut self, quote: u8) -> Tok {
        let start = self.pos;
        self.pos += 1;
        while self.pos < self.src.len() && self.src[self.pos] != quote {
            self.pos += 1;
        }
        let raw = &self.text[start + 1..self.pos.min(self.src.len())];
        let value = decode_entities(raw);
        if self.pos >= self.src.len() {
            self.error("unterminated string literal", start, start + 1);
        } else {
            self.pos += 1;
        }
        Tok::Str(self.push_payload(Payload::Text(value)))
    }

    /// One token between the tags: text, `{`, `<` or `</`.
    pub(super) fn jsx_children_token(&mut self) -> Tok {
        match (self.at(0), self.at(1)) {
            (b'{', _) => {
                self.pos += 1;
                self.push_mode(Mode::JsxExpr);
                Tok::LBrace
            }
            (b'<', b'/') => {
                self.pos += 2;
                self.push_mode(Mode::JsxTag { closing: true });
                Tok::JsxLtSlash
            }
            (b'<', _) => {
                self.pos += 1;
                self.push_mode(Mode::JsxTag { closing: false });
                Tok::JsxLt
            }
            _ => self.jsx_text(),
        }
    }

    /// Text up to the next `{` or `<`. A `>` or `}` in it is an error, as in TypeScript.
    fn jsx_text(&mut self) -> Tok {
        let start = self.pos;
        while self.pos < self.src.len() && !matches!(self.src[self.pos], b'{' | b'<') {
            let c = self.src[self.pos];
            if c == b'>' || c == b'}' {
                let msg = format!(
                    "Unexpected token. Did you mean `{{'{}'}}` or `{}`?",
                    c as char,
                    if c == b'>' { "&gt;" } else { "&rbrace;" }
                );
                self.error(msg, self.pos, self.pos + 1);
            }
            self.pos += 1;
        }
        let value = decode_entities(&clean_text(&self.text[start..self.pos]));
        Tok::JsxText(self.push_payload(Payload::Text(value)))
    }

    /// Records `@jsxImportSource pkg` from a comment at `start..self.pos` if no token precedes it.
    pub(super) fn leading_pragma(&mut self, start: usize) {
        if !self.toks.is_empty() || self.jsx_import_source.is_some() {
            return;
        }
        let comment = &self.text[start..self.pos];
        let Some(at) = comment.find("@jsxImportSource") else {
            return;
        };
        let rest = &comment[at + "@jsxImportSource".len()..];
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            return;
        }
        let value: String = rest
            .trim_start()
            .chars()
            .take_while(|&c| !c.is_whitespace() && c != '*')
            .collect();
        if !value.is_empty() {
            self.jsx_import_source = Some(value);
        }
    }
}

/// React's JSX whitespace rules (Babel's `cleanJSXElementLiteralChild`): tabs count as spaces;
/// every line but the first loses its leading spaces and every line but the last its trailing
/// ones; whitespace-only lines are dropped and the remaining lines are joined with one space.
/// Whitespace within a line is kept as written.
pub(crate) fn clean_text(raw: &str) -> String {
    let unified = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = unified.split(['\n', '\r']).collect();
    let last_non_empty = lines
        .iter()
        .rposition(|l| l.chars().any(|c| c != ' ' && c != '\t'))
        .unwrap_or(0);
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let spaced = line.replace('\t', " ");
        let mut trimmed = spaced.as_str();
        if i != 0 {
            trimmed = trimmed.trim_start_matches(' ');
        }
        if i != lines.len() - 1 {
            trimmed = trimmed.trim_end_matches(' ');
        }
        if !trimmed.is_empty() {
            out.push_str(trimmed);
            if i != last_non_empty {
                out.push(' ');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::clean_text;

    #[test]
    fn whitespace_rules_match_react() {
        assert_eq!(clean_text("a"), "a");
        assert_eq!(clean_text("  a  "), "  a  ");
        assert_eq!(clean_text("\n  a\n  b\n"), "a b");
        assert_eq!(clean_text("a   b"), "a   b");
        assert_eq!(clean_text("\n   \n"), "");
        assert_eq!(clean_text(" "), " ");
        assert_eq!(clean_text("a \n "), "a");
        assert_eq!(clean_text("\r\n a\r\n\tb \r\n"), "a b");
        assert_eq!(clean_text("x\t\ty"), "x  y");
    }
}
