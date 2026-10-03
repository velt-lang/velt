//! `JsonError.message` texts. Formats (documented in rt_abi_async.md §12.4):
//! - type mismatch (decoder wanted another kind): `expected <expected> at <path>`
//! - syntax error (typed decoding and `JSON.parseValue` alike):
//!   `invalid JSON at <path>: <detail> (byte <offset>)`
//! - nesting past the depth limit: `JSON nested deeper than <limit> levels at <path> (byte <offset>)`
//! - an unknown key, rejected: `unknown field at <path>`
//!
//! A path longer than [`PATH_KEEP`] segments at each end is shortened to them with `…` between
//! (`$[0][0]…[0].name`), so a deep document gives a readable message. Paths are only built
//! once decoding has failed, so this costs nothing on success.

use super::scan::{SyntaxError, TOO_DEEP, UNEXPECTED_CHAR};
use std::borrow::Cow;

/// Path segments kept at each end of a shortened path.
pub const PATH_KEEP: usize = 10;

/// `path` with at most `2 * PATH_KEEP` segments (each starts at a `.` or a `[`): a longer one
/// keeps its first and last `PATH_KEEP` with `…` between.
pub fn short_path(path: &str) -> Cow<'_, str> {
    let starts: Vec<usize> = path
        .bytes()
        .enumerate()
        .filter(|&(_, b)| b == b'.' || b == b'[')
        .map(|(i, _)| i)
        .collect();
    if starts.len() <= 2 * PATH_KEEP {
        return Cow::Borrowed(path);
    }
    // `.` and `[` are ASCII, so both cuts are on character boundaries.
    let head = &path[..starts[PATH_KEEP]];
    let tail = &path[starts[starts.len() - PATH_KEEP]..];
    Cow::Owned(format!("{head}…{tail}"))
}

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

/// Message for a syntax error found at `path` (by the pull reader or `JSON.parseValue`);
/// `max_depth` is the nesting limit, for a [`TOO_DEEP`] error.
pub fn syntax_message(src: &[u8], e: SyntaxError, path: &str, max_depth: usize) -> String {
    let path = short_path(path);
    if e.what == TOO_DEEP {
        return format!(
            "JSON nested deeper than {max_depth} levels at {path} (byte {})",
            e.at
        );
    }
    format!("invalid JSON at {path}: {} (byte {})", detail(src, e), e.at)
}

/// Message for a decoder type mismatch.
pub fn mismatch_message(expected: &str, path: &str) -> String {
    format!("expected {expected} at {}", short_path(path))
}

/// Message for an object key the target type has no field for (unknown keys rejected).
pub fn unknown_message(path: &str) -> String {
    format!("unknown field at {}", short_path(path))
}

#[cfg(test)]
mod tests {
    use super::short_path;

    #[test]
    fn long_paths_keep_both_ends() {
        assert_eq!(short_path("$"), "$");
        assert_eq!(short_path("$.a[1].b"), "$.a[1].b");
        let twenty = "$".to_string() + &"[0]".repeat(20);
        assert_eq!(short_path(&twenty), twenty);
        let deep = "$".to_string() + &"[0]".repeat(30) + ".name";
        let want = "$".to_string() + &"[0]".repeat(10) + "…" + &"[0]".repeat(9) + ".name";
        assert_eq!(short_path(&deep), want);
        // Keys with non-ASCII text are cut only at segment starts.
        let keys = "$".to_string() + &".é".repeat(25);
        let want = "$".to_string() + &".é".repeat(10) + "…" + &".é".repeat(10);
        assert_eq!(short_path(&keys), want);
    }
}
