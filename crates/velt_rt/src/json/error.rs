//! `JsonError.message` texts. Formats (documented in rt_abi_async.md §12.4):
//! - type mismatch (decoder wanted another kind): `expected <expected> at <path>`
//! - syntax error while decoding: `invalid JSON at <path>: <detail> (byte <offset>)`
//! - syntax error in `JSON.parseValue`: `invalid JSON: <detail> (byte <offset>)`

use super::scan::{SyntaxError, UNEXPECTED_CHAR};

/// `<detail>`, naming the offending character for [`UNEXPECTED_CHAR`] (`U+XXXX` if it is a
/// control character).
fn detail(src: &[u8], e: SyntaxError) -> String {
    if e.what != UNEXPECTED_CHAR {
        return e.what.to_string();
    }
    let rest = &src[e.at.min(src.len())..];
    let ch = std::str::from_utf8(&rest[..rest.len().min(4)])
        .or_else(|err| std::str::from_utf8(&rest[..err.valid_up_to()]))
        .ok()
        .and_then(|s| s.chars().next());
    match ch {
        Some(c) if !c.is_control() => format!("{UNEXPECTED_CHAR} '{c}'"),
        Some(c) => format!("{UNEXPECTED_CHAR} U+{:04X}", c as u32),
        None => UNEXPECTED_CHAR.to_string(),
    }
}

/// Message for a syntax error found by the pull reader at `path`.
pub fn reader_syntax_message(src: &[u8], e: SyntaxError, path: &str) -> String {
    format!("invalid JSON at {path}: {} (byte {})", detail(src, e), e.at)
}

/// Message for a syntax error found by `JSON.parseValue`.
pub fn value_syntax_message(src: &[u8], e: SyntaxError) -> String {
    format!("invalid JSON: {} (byte {})", detail(src, e), e.at)
}

/// Message for a decoder type mismatch.
pub fn mismatch_message(expected: &str, path: &str) -> String {
    format!("expected {expected} at {path}")
}
