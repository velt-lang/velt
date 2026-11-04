//! Suppression comments (docs/internals/design/lint-profiles.md §5):
//!
//! ```ts ignore
//! // velt-allow(perf/number-double): the total must stay a JS number for the wire format
//! let total = 0;
//! let t = 0; // velt-allow(perf/number-double): same
//! // velt-allow-file(ts-compat/velt-member): this file is only compiled by velt
//! ```
//!
//! A `velt-allow` comment alone on its line covers the next line with code; after code, its own
//! line. `velt-allow-file` covers the file. Several codes are separated by commas; `perf/*`
//! covers a family. The reason after `:` is required, so a suppression explains itself; an
//! allow without one, of an unknown code, or that covers nothing is a warning.

use velt_common::{Diagnostic, FileId, Span};

use crate::rules;

/// One suppression comment.
#[derive(Debug)]
struct Allow {
    codes: Vec<String>,
    /// The line it covers (0-based), `None` for the whole file.
    line: Option<usize>,
    /// The comment.
    span: Span,
    used: bool,
}

/// The suppression comments of one file.
#[derive(Debug, Default)]
pub struct Allows {
    allows: Vec<Allow>,
    /// The file's line starts, for the line of a finding.
    lines: Vec<u32>,
    /// Comments that don't parse, or have no reason.
    problems: Vec<Diagnostic>,
}

impl Allows {
    /// The suppression comments of `src` (the file `file`).
    pub fn parse(file: FileId, src: &str) -> Allows {
        let mut out = Allows {
            lines: line_starts(src),
            ..Allows::default()
        };
        // Parsing the file for its comments costs a parse: only when there may be one.
        if !src.contains("velt-allow") {
            return out;
        }
        for range in velt_syntax::comment_ranges(src) {
            let text = &src[range.start as usize..range.end as usize];
            let Some(body) = text.strip_prefix("//") else {
                continue;
            };
            let span = Span::new(file, range.start, range.end);
            let body = body.trim_start();
            let (file_wide, rest) = match body.strip_prefix("velt-allow-file(") {
                Some(rest) => (true, rest),
                None => match body.strip_prefix("velt-allow(") {
                    Some(rest) => (false, rest),
                    None => continue,
                },
            };
            let Some((codes, after)) = rest.split_once(')') else {
                out.problem("this `velt-allow` comment has no closing `)`", span);
                continue;
            };
            let codes: Vec<String> = codes.split(',').map(|c| c.trim().to_string()).collect();
            let reason = after.trim_start().strip_prefix(':').map(str::trim);
            if reason.is_none_or(str::is_empty) {
                out.problem_with(
                    "this `velt-allow` comment gives no reason",
                    span,
                    "write why after a colon: `// velt-allow(perf/number-double): <why>`",
                );
            }
            for code in codes.iter().filter(|c| !rules::is_known(c)) {
                out.problem(format!("`{code}` is not a lint rule"), span);
            }
            let line = (!file_wide).then(|| out.covered_line(src, range.start as usize));
            out.allows.push(Allow {
                codes,
                line,
                span,
                used: false,
            });
        }
        out
    }

    /// Whether a finding of `code` at `span` is suppressed (and the comment that does so is then
    /// used).
    pub fn suppresses(&mut self, code: &str, span: Span) -> bool {
        let line = self.line_of(span.lo);
        let covers = |a: &Allow| {
            (a.line.is_none() || a.line == Some(line)) && a.codes.iter().any(|c| matches(c, code))
        };
        match self.allows.iter_mut().find(|a| covers(a)) {
            Some(a) => {
                a.used = true;
                true
            }
            None => false,
        }
    }

    /// The warnings about the comments: those that don't parse or give no reason, and those that
    /// suppressed nothing although their rules ran (`ran` of a code).
    pub fn finish(self, ran: &dyn Fn(&str) -> bool) -> Vec<Diagnostic> {
        let mut out = self.problems;
        for a in self.allows.iter().filter(|a| !a.used) {
            if a.codes
                .iter()
                .all(|c| rules::is_known(c) && ran_any(c, ran))
            {
                out.push(Diagnostic::warning(
                    format!(
                        "this `velt-allow({})` suppresses nothing",
                        a.codes.join(", ")
                    ),
                    a.span,
                ));
            }
        }
        out
    }

    fn problem(&mut self, message: impl Into<String>, span: Span) {
        self.problems.push(Diagnostic::warning(message, span));
    }

    fn problem_with(&mut self, message: &str, span: Span, note: &str) {
        self.problems
            .push(Diagnostic::warning(message, span).with_note(note));
    }

    /// The line a comment starting at `at` covers: its own when code comes before it, else the
    /// next line that is not blank or only a comment.
    fn covered_line(&self, src: &str, at: usize) -> usize {
        let line = self.line_of(at as u32);
        let start = self.lines[line] as usize;
        if !src[start..at].trim().is_empty() {
            return line;
        }
        let mut next = line + 1;
        while let Some(&lo) = self.lines.get(next) {
            let hi = self.lines.get(next + 1).map_or(src.len(), |&h| h as usize);
            let text = src[lo as usize..hi].trim();
            if !text.is_empty() && !text.starts_with("//") {
                break;
            }
            next += 1;
        }
        next
    }

    fn line_of(&self, offset: u32) -> usize {
        self.lines
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }
}

/// Whether a rule `pattern` covers ran: the rule itself, or for `family/*` any of the family's.
fn ran_any(pattern: &str, ran: &dyn Fn(&str) -> bool) -> bool {
    match pattern.strip_suffix("/*") {
        Some("perf") => rules::PERF_RULES.iter().any(|r| ran(r.code)),
        Some("ts-compat") => velt_tscompat::RULES
            .iter()
            .any(|r| ran(&rules::ts_compat_code(r))),
        Some(_) => false,
        None => ran(pattern),
    }
}

/// Whether the allowed `pattern` (a code, or `family/*`) covers `code`.
fn matches(pattern: &str, code: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(family) => code
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with('/')),
        None => pattern == code,
    }
}

fn line_starts(src: &str) -> Vec<u32> {
    let mut out = vec![0];
    out.extend(
        src.bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i as u32 + 1),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(src: &str, needle: &str) -> Span {
        let lo = src.find(needle).unwrap() as u32;
        Span::new(FileId(0), lo, lo + needle.len() as u32)
    }

    #[test]
    fn covers_the_next_line_or_its_own() {
        let src = "// velt-allow(perf/number-double): why\n\nlet a = 0;\nlet b = 0; // velt-allow(perf/*): why\nlet c = 0;\n";
        let mut allows = Allows::parse(FileId(0), src);
        assert!(allows.suppresses("perf/number-double", at(src, "a = 0")));
        assert!(allows.suppresses("perf/json-parse-cast", at(src, "b = 0")));
        assert!(!allows.suppresses("perf/number-double", at(src, "c = 0")));
        assert!(allows.finish(&|_| true).is_empty());
    }

    #[test]
    fn file_wide_and_problems() {
        let src = "// velt-allow-file(ts-compat/bool-type)\n// velt-allow(perf/nope): x\nlet a = 0;\n// velt-allow(perf/number-double): unused\nlet b = 0;\n";
        let mut allows = Allows::parse(FileId(0), src);
        assert!(allows.suppresses("ts-compat/bool-type", at(src, "b = 0")));
        let messages: Vec<String> = allows
            .finish(&|_| true)
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert_eq!(
            messages,
            [
                "this `velt-allow` comment gives no reason",
                "`perf/nope` is not a lint rule",
                "this `velt-allow(perf/number-double)` suppresses nothing",
            ]
        );
    }

    /// A family's wildcard that covers nothing is unused once one of its rules ran.
    #[test]
    fn an_unused_wildcard_is_reported() {
        let src = "// velt-allow(perf/*): the whole family\nconst y = 1;\n";
        let allows = Allows::parse(FileId(0), src);
        let ran = |c: &str| c == "perf/foreach-capture";
        let messages: Vec<String> = allows.finish(&ran).into_iter().map(|d| d.message).collect();
        assert_eq!(messages, ["this `velt-allow(perf/*)` suppresses nothing"]);
        let allows = Allows::parse(FileId(0), src);
        assert!(allows.finish(&|c| c.starts_with("ts-compat/")).is_empty());
    }

    #[test]
    fn rules_that_did_not_run_are_not_unused() {
        let src = "// velt-allow(perf/number-double): why\nlet a = 0;\n";
        let allows = Allows::parse(FileId(0), src);
        assert!(allows.finish(&|_| false).is_empty());
    }

    #[test]
    fn a_string_is_not_a_comment() {
        let src = "const s = \"// velt-allow(perf/number-double): x\";\n";
        let mut allows = Allows::parse(FileId(0), src);
        assert!(!allows.suppresses("perf/number-double", at(src, "const")));
    }
}
