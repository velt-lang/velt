//! Comment recovery. The AST drops comments, so this module asks the lexer where they are
//! ([`velt_syntax::comment_ranges`]) and hands them out in source order as the printer walks the AST:
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

/// Every comment in `src`, in source order (the lexer's view, so `//` inside a string, a
/// template, a regular expression or JSX text is not a comment).
pub(crate) fn scan(src: &str) -> Vec<Comment> {
    velt_syntax::comment_ranges(src)
        .into_iter()
        .map(|r| {
            let (lo, hi) = (r.start as usize, r.end as usize);
            comment(src, lo, hi, src[lo..].starts_with("/*"))
        })
        .collect()
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
        let src = "const r = /\"/; const j = <p a='//'>don't // no {/* yes */}</p>; // yes\n";
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
