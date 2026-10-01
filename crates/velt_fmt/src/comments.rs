//! Comment recovery. The lexer drops comments, so this module re-scans the source for them
//! (skipping string and template literals, including `${ }` substitutions, so `//` inside a
//! string is not a comment) and hands them out in source order as the printer walks the AST:
//! [`Comments::take_before`] yields the comments preceding a node (leading comments) and
//! [`Comments::take_trailing`] those that end the line a node ends on.

/// One comment in the source.
#[derive(Clone, Debug)]
pub(crate) struct Comment {
    /// Byte offset of `//` or `/*`.
    pub lo: u32,
    /// Byte offset just past the comment (line comments exclude the newline).
    pub hi: u32,
    /// The comment text; line comments without trailing whitespace.
    pub text: String,
    /// `/* ... */` rather than `// ...`.
    pub is_block: bool,
    /// Only whitespace follows the comment up to the end of its line.
    pub newline_after: bool,
}

impl Comment {
    /// Must be followed by a line break when printed.
    pub(crate) fn needs_newline(&self) -> bool {
        !self.is_block || self.newline_after
    }
}

/// Every comment in `src`, in source order.
pub(crate) fn scan(src: &str) -> Vec<Comment> {
    let bytes = src.as_bytes();
    let mut out = vec![];
    // One entry per open `{`; `true` = it opened a template substitution `${`.
    let mut braces: Vec<bool> = vec![];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => i = skip_string(bytes, i),
            b'`' => i = skip_template(bytes, i + 1, &mut braces),
            b'{' => {
                braces.push(false);
                i += 1;
            }
            b'}' => {
                i += 1;
                if braces.pop() == Some(true) {
                    i = skip_template(bytes, i, &mut braces);
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = src[i..].find('\n').map_or(src.len(), |n| i + n);
                out.push(comment(src, i, end, false));
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..].find("*/").map_or(src.len(), |n| i + 2 + n + 2);
                out.push(comment(src, i, end, true));
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

fn comment(src: &str, lo: usize, hi: usize, is_block: bool) -> Comment {
    let raw = &src[lo..hi];
    let text = if is_block { raw } else { raw.trim_end() };
    let rest = src[hi..].trim_start_matches([' ', '\t']);
    Comment {
        lo: lo as u32,
        hi: hi as u32,
        text: text.to_string(),
        is_block,
        newline_after: rest.is_empty() || rest.starts_with(['\n', '\r']),
    }
}

/// Index just past the string literal starting at `start` (strings end at a newline too).
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\n' => return i,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Scans template text from `i` to the closing backtick or the next `${` (which is pushed on the
/// brace stack so the matching `}` resumes the template).
fn skip_template(bytes: &[u8], mut i: usize, braces: &mut Vec<bool>) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => return i + 1,
            b'$' if bytes.get(i + 1) == Some(&b'{') => {
                braces.push(true);
                return i + 2;
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The comments of one file, handed out in source order.
pub(crate) struct Comments {
    list: Vec<Comment>,
    next: usize,
}

impl Comments {
    pub(crate) fn new(src: &str) -> Comments {
        Comments {
            list: scan(src),
            next: 0,
        }
    }

    /// All not-yet-printed comments starting before `pos`.
    pub(crate) fn take_before(&mut self, pos: u32) -> Vec<Comment> {
        let start = self.next;
        while self.next < self.list.len() && self.list[self.next].lo < pos {
            self.next += 1;
        }
        self.list[start..self.next].to_vec()
    }

    /// Comments before `limit` that sit on the line where a node ending at `end` ends. Unless
    /// `inline_blocks`, a block comment with more code after it on its line is left for the
    /// next node (`f(a, /* b */ b)`).
    pub(crate) fn take_trailing(
        &mut self,
        src: &str,
        mut end: u32,
        limit: u32,
        inline_blocks: bool,
    ) -> Vec<Comment> {
        let mut out = vec![];
        while let Some(c) = self.list.get(self.next) {
            let gap = src.get(end.min(c.lo) as usize..c.lo as usize).unwrap_or("");
            let leads_next = !inline_blocks && !c.needs_newline();
            if c.lo >= limit || gap.contains('\n') || leads_next {
                break;
            }
            end = c.hi;
            out.push(c.clone());
            self.next += 1;
        }
        out
    }

    /// Marks the comments before `hi` as printed: the caller has already taken the ones before
    /// the verbatim text that ends at `hi`, and the rest are part of that text.
    pub(crate) fn skip_until(&mut self, hi: u32) {
        while self.next < self.list.len() && self.list[self.next].lo < hi {
            self.next += 1;
        }
    }
}

/// Number of comments in `src` (used to check that formatting preserves them).
pub fn count(src: &str) -> usize {
    scan(src).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comment_markers_in_literals() {
        let src = "let a = \"// no\"; let b = `/* no ${ {x: 1} /* yes */ } // no`; // yes\n";
        let found: Vec<String> = scan(src).into_iter().map(|c| c.text).collect();
        assert_eq!(found, ["/* yes */", "// yes"]);
    }

    #[test]
    fn records_line_endings() {
        let cs = scan("/* a */ x;\n/* b */\n// c   \n");
        assert!(!cs[0].newline_after && cs[1].newline_after);
        assert_eq!(cs[2].text, "// c");
    }

    #[test]
    fn hands_out_in_order() {
        let src = "a; // t\n// l\nb;";
        let mut c = Comments::new(src);
        assert_eq!(c.take_trailing(src, 2, 100, true).len(), 1);
        assert_eq!(c.take_trailing(src, 7, 100, true).len(), 0);
        assert_eq!(c.take_before(13).len(), 1);
    }
}
