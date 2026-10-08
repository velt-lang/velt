//! Doc comments: finding the one that documents a declaration, and reading its tags.
//!
//! A doc comment is a JSDoc block (`/** … */`) or a block of `///` lines that ends on the line
//! right above a declaration (or on the declaration's own line, before it). A plain `//` or
//! `/* */` comment is not a doc comment, and neither is one separated from the declaration by a
//! blank line; plain comments on the lines in between are skipped, as in TypeScript. Comments
//! are found by the lexer ([`velt_syntax::comment_ranges`]), so `//` inside a string or
//! template is never mistaken for one.
//!
//! The text is Markdown. JSDoc tags are read into [`DocComment`]'s fields: `@param`,
//! `@returns` (`@return`), `@throws`, `@example`, `@deprecated` and `@see`; `{@link x}` becomes
//! `` `x` ``. Other tags stay in the text. Design: `docs/internals/design/doc-comments.md`.

use std::ops::Range;

/// A parsed doc comment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocComment {
    /// The description: Markdown, without the comment markers and the tags.
    pub body: String,
    /// `@param name text`, in order: `(name, text)`.
    pub params: Vec<(String, String)>,
    /// `@returns text`.
    pub returns: Option<String>,
    /// One entry per `@throws`.
    pub throws: Vec<String>,
    /// One entry per `@example`, verbatim (code, or Markdown with its own code fence).
    pub examples: Vec<String>,
    /// `@deprecated [text]`: `Some("")` without text.
    pub deprecated: Option<String>,
    /// One entry per `@see`.
    pub see: Vec<String>,
}

impl DocComment {
    /// Nothing documented.
    pub fn is_empty(&self) -> bool {
        *self == DocComment::default()
    }

    /// The text of `@param name`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, text)| text.as_str())
    }

    /// The whole comment as Markdown: the body, then the parameters, return value, exceptions,
    /// examples (in ` ```ts ` fences unless they bring their own), deprecation and links.
    pub fn render_markdown(&self) -> String {
        let mut parts: Vec<String> = vec![];
        if !self.body.is_empty() {
            parts.push(self.body.clone());
        }
        if !self.params.is_empty() {
            let items: Vec<String> = self
                .params
                .iter()
                .map(|(name, text)| match text.is_empty() {
                    true => format!("- `{name}`"),
                    false => format!("- `{name}`: {}", indent_rest(text)),
                })
                .collect();
            parts.push(format!("**Parameters**\n\n{}", items.join("\n")));
        }
        if let Some(text) = &self.returns {
            parts.push(format!("**Returns:** {text}"));
        }
        if let Some(section) = labelled_list("Throws", &self.throws) {
            parts.push(section);
        }
        for example in &self.examples {
            if example.contains("```") {
                parts.push(format!("**Example**\n\n{example}"));
            } else {
                parts.push(format!("**Example**\n\n```ts\n{example}\n```"));
            }
        }
        if let Some(text) = &self.deprecated {
            parts.push(match text.is_empty() {
                true => "**Deprecated.**".to_string(),
                false => format!("**Deprecated:** {text}"),
            });
        }
        if let Some(section) = labelled_list("See also", &self.see) {
            parts.push(section);
        }
        parts.join("\n\n")
    }
}

/// `**Label:** text` for one entry, a list under `**Label**` for several.
fn labelled_list(label: &str, entries: &[String]) -> Option<String> {
    match entries {
        [] => None,
        [one] => Some(format!("**{label}:** {one}")),
        many => {
            let items: Vec<String> = many
                .iter()
                .map(|e| format!("- {}", indent_rest(e)))
                .collect();
            Some(format!("**{label}**\n\n{}", items.join("\n")))
        }
    }
}

/// Indents the lines after the first by two spaces (continuation lines of a list item).
fn indent_rest(text: &str) -> String {
    text.replace('\n', "\n  ")
}

/// The doc comment of the declaration starting at byte `decl_lo` of `src` (where `export`, a
/// modifier or the keyword starts). Lexes `src`; with many declarations, find the comments once
/// and use [`doc_before_in`].
pub fn doc_before(src: &str, decl_lo: u32) -> Option<DocComment> {
    doc_before_in(src, &velt_syntax::comment_ranges(src), decl_lo)
}

/// [`doc_before`] with the comment ranges of `src` ([`velt_syntax::comment_ranges`]).
pub fn doc_before_in(src: &str, comments: &[Range<u32>], decl_lo: u32) -> Option<DocComment> {
    let range = doc_range_before(src, comments, decl_lo)?;
    Some(parse(src.get(range.start as usize..range.end as usize)?))
}

/// The byte range of the doc comment of the declaration at `decl_lo`: one `/** … */` comment,
/// or a block of `///` lines. Plain comments on the lines between it and the declaration
/// (`// eslint-disable-next-line`, `// @ts-expect-error`) are skipped, as TypeScript does.
pub fn doc_range_before(src: &str, comments: &[Range<u32>], decl_lo: u32) -> Option<Range<u32>> {
    let k = comments.partition_point(|c| c.start < decl_lo);
    let last = comments.get(k.checked_sub(1)?)?;
    if last.end > decl_lo || !next_to_declaration(src.get(last.end as usize..decl_lo as usize)?) {
        return None;
    }
    // The closest comment that is not a plain one on its own line, if line-adjacent ones lead
    // to it.
    let mut i = k - 1;
    loop {
        let c = &comments[i];
        if !starts_line(src, c.start) {
            return None; // a trailing comment of the code before it
        }
        let text = src.get(c.start as usize..c.end as usize)?;
        if is_jsdoc(text) {
            return Some(c.clone());
        }
        if is_triple_slash(text) {
            break;
        }
        let above = comments.get(i.checked_sub(1)?)?;
        if !line_adjacent(src, above, c.start) {
            return None;
        }
        i -= 1;
    }
    let mut start = comments[i].start;
    for c in comments[..i].iter().rev() {
        let text = src.get(c.start as usize..c.end as usize)?;
        if !line_adjacent(src, c, start) || !is_triple_slash(text) || !starts_line(src, c.start) {
            break;
        }
        start = c.start;
    }
    Some(start..comments[i].end)
}

/// A plain comment (not a doc comment) on its own line ends right above the declaration at
/// `decl_lo`.
pub fn plain_comment_before(src: &str, comments: &[Range<u32>], decl_lo: u32) -> bool {
    let k = comments.partition_point(|c| c.start < decl_lo);
    let Some(last) = k.checked_sub(1).and_then(|i| comments.get(i)) else {
        return false;
    };
    let text = src
        .get(last.start as usize..last.end as usize)
        .unwrap_or("");
    last.end <= decl_lo
        && src
            .get(last.end as usize..decl_lo as usize)
            .is_some_and(next_to_declaration)
        && starts_line(src, last.start)
        && !is_jsdoc(text)
        && !is_triple_slash(text)
}

/// Comment `c` ends on the line before the one where byte `next` starts.
fn line_adjacent(src: &str, c: &Range<u32>, next: u32) -> bool {
    src.get(c.end as usize..next as usize)
        .is_some_and(|gap| gap.trim().is_empty() && gap.matches('\n').count() == 1)
}

/// The text between a doc comment and its declaration: whitespace with at most one line break.
fn next_to_declaration(gap: &str) -> bool {
    gap.trim().is_empty() && gap.matches('\n').count() <= 1
}

/// Only whitespace precedes byte `lo` on its line.
fn starts_line(src: &str, lo: u32) -> bool {
    let before = src.get(..lo as usize).unwrap_or("");
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    before[line_start..].trim().is_empty()
}

/// `/** … */` (not `/**/` or `/*** … */`).
fn is_jsdoc(text: &str) -> bool {
    text.starts_with("/**") && !text.starts_with("/***") && !text.starts_with("/**/")
}

/// `/// …` (not `////`).
fn is_triple_slash(text: &str) -> bool {
    text.starts_with("///") && !text.starts_with("////")
}

/// The module doc of `src`: its leading comment block (comments on consecutive lines, any
/// style), when a blank line or the end of the file follows it.
pub fn module_doc(src: &str, comments: &[Range<u32>]) -> Option<DocComment> {
    let first = comments.first()?;
    let lead = src.get(..first.start as usize)?;
    let lead = match lead.strip_prefix("#!") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, r)| r),
        None => lead,
    };
    if !lead.trim().is_empty() {
        return None;
    }
    let mut n = 1;
    while let Some(c) = comments.get(n) {
        let gap = src.get(comments[n - 1].end as usize..c.start as usize)?;
        if !gap.trim().is_empty() || gap.matches('\n').count() != 1 {
            break;
        }
        n += 1;
    }
    let after = src.get(comments[n - 1].end as usize..)?;
    let rest = after.trim_start_matches([' ', '\t', '\r']);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let blank_follows =
        rest.trim().is_empty() || rest.split('\n').next().is_some_and(|l| l.trim().is_empty());
    if !blank_follows {
        return None;
    }
    let mut lines = vec![];
    for c in &comments[..n] {
        let text = src.get(c.start as usize..c.end as usize)?;
        match text.starts_with("/*") {
            true => lines.extend(strip_markers(text)),
            // One line each, empty ones included (they separate paragraphs).
            false => lines.push(strip_line(text)),
        }
    }
    Some(parse_lines(&lines))
}

/// Parses the text of a doc comment: a `/** … */` (or `/* … */`) block, or `///` or `//`
/// lines, markers included.
pub fn parse(comment_text: &str) -> DocComment {
    parse_lines(&strip_markers(comment_text))
}

/// The lines of a comment without its markers: `/**`, `*/`, a leading ` * ` on each line, or
/// `///` / `//` and one space. Indentation beyond that is kept.
fn strip_markers(text: &str) -> Vec<String> {
    let text = text.trim();
    if let Some(inner) = text.strip_prefix("/*") {
        let inner = inner.strip_prefix('*').unwrap_or(inner);
        let inner = inner.strip_suffix("*/").unwrap_or(inner);
        let raw: Vec<&str> = inner.lines().collect();
        // Lines without a leading `*` lose their common indentation.
        let indent = raw
            .iter()
            .skip(1)
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('*'))
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let mut lines: Vec<String> = raw
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let t = l.trim_start();
                if i == 0 {
                    t.to_string()
                } else if let Some(rest) = t.strip_prefix('*') {
                    rest.strip_prefix(' ').unwrap_or(rest).to_string()
                } else {
                    l.get(indent..).unwrap_or(t).to_string()
                }
            })
            .map(|l| l.trim_end().to_string())
            .collect();
        trim_blank_lines(&mut lines);
        return lines;
    }
    let mut lines: Vec<String> = text.lines().map(strip_line).collect();
    trim_blank_lines(&mut lines);
    lines
}

/// A `///` or `//` line without the marker and one space.
fn strip_line(line: &str) -> String {
    let l = line.trim();
    let l = l
        .strip_prefix("///")
        .or_else(|| l.strip_prefix("//"))
        .unwrap_or(l);
    l.strip_prefix(' ').unwrap_or(l).trim_end().to_string()
}

fn trim_blank_lines(lines: &mut Vec<String>) {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| l.trim().is_empty()).count();
    lines.drain(..lead);
}

/// Splits comment lines into the body and the tags.
fn parse_lines(lines: &[String]) -> DocComment {
    let mut doc = DocComment::default();
    let mut body: Vec<String> = vec![];
    // The tag being read: its name and its lines (the first is the text after the name).
    let mut tag: Option<(String, Vec<String>)> = None;
    let mut in_fence = false;
    for line in lines {
        let t = line.trim_start();
        let starts_tag = !in_fence
            && t.starts_with('@')
            && t[1..].starts_with(|c: char| c.is_ascii_alphabetic());
        if t.starts_with("```") {
            in_fence = !in_fence;
        }
        if starts_tag {
            if let Some((name, text)) = tag.take() {
                finish_tag(&mut doc, &mut body, &name, text);
            }
            let rest = &t[1..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            let first = rest[end..].strip_prefix(' ').unwrap_or(&rest[end..]);
            tag = Some((rest[..end].to_string(), vec![first.to_string()]));
        } else if let Some((_, text)) = &mut tag {
            text.push(line.clone());
        } else {
            body.push(line.clone());
        }
    }
    if let Some((name, text)) = tag.take() {
        finish_tag(&mut doc, &mut body, &name, text);
    }
    trim_blank_lines(&mut body);
    doc.body = links(&body.join("\n"));
    doc
}

/// Stores tag `name` with its `lines`.
fn finish_tag(doc: &mut DocComment, body: &mut Vec<String>, name: &str, mut lines: Vec<String>) {
    if name == "example" {
        trim_blank_lines(&mut lines);
        doc.examples.push(lines.join("\n"));
        return;
    }
    let text = |lines: &[String]| -> String {
        let joined: Vec<&str> = lines.iter().map(|l| l.trim()).collect();
        links(joined.join("\n").trim())
    };
    match name {
        "param" | "arg" | "argument" => {
            let first = skip_type(&lines[0]).0;
            let (name, rest) = param_name(first);
            lines[0] = rest.to_string();
            doc.params.push((name, text(&lines)));
        }
        "returns" | "return" => {
            lines[0] = skip_type(&lines[0]).0.to_string();
            doc.returns = Some(text(&lines));
        }
        "throws" | "throw" | "exception" => {
            let (rest, ty) = skip_type(&lines[0]);
            let (rest, ty) = (rest.to_string(), ty.map(str::to_string));
            lines[0] = rest;
            let text = text(&lines);
            doc.throws.push(match (ty, text.is_empty()) {
                (Some(ty), true) => format!("`{ty}`"),
                (Some(ty), false) => format!("`{ty}`: {text}"),
                (None, _) => text,
            });
        }
        "deprecated" => doc.deprecated = Some(text(&lines)),
        "see" => doc.see.push(text(&lines)),
        _ => {
            // An unknown tag is kept as written.
            if body.last().is_some_and(|l| !l.trim().is_empty()) {
                body.push(String::new());
            }
            body.push(format!("@{name} {}", lines[0]).trim_end().to_string());
            body.extend(lines.into_iter().skip(1));
        }
    }
}

/// Skips a leading `{type}` (braces may nest): `(rest, type)`.
fn skip_type(text: &str) -> (&str, Option<&str>) {
    let t = text.trim_start();
    if !t.starts_with('{') {
        return (t, None);
    }
    let mut depth = 0;
    for (i, c) in t.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (t[i + 1..].trim_start(), Some(t[1..i].trim()));
                }
            }
            _ => {}
        }
    }
    (t, None)
}

/// A `@param`'s name (`name`, `[name]` or `[name=default]`) and the text after it and an
/// optional `-`.
fn param_name(text: &str) -> (String, &str) {
    let (name, rest) = if let Some(inner) = text.strip_prefix('[') {
        // The `]` that closes this one: a default may hold brackets (`[opts=[]]`).
        let mut depth = 0;
        let end = inner
            .char_indices()
            .find(|&(_, c)| {
                match c {
                    '[' => depth += 1,
                    ']' if depth == 0 => return true,
                    ']' => depth -= 1,
                    _ => {}
                }
                false
            })
            .map_or(inner.len(), |(i, _)| i);
        let name = inner[..end].split('=').next().unwrap_or("").trim();
        (name, inner.get(end + 1..).unwrap_or(""))
    } else {
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        (&text[..end], &text[end..])
    };
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('-') {
        Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => r.trim_start(),
        _ => rest,
    };
    (name.to_string(), rest)
}

/// Replaces inline `{@link target}` and `{@link target text}` (also `target | text`,
/// `{@linkcode}`, `{@linkplain}`) outside code: `` `target` ``, the text, or a Markdown link
/// for a URL.
fn links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if in_fence || !line.contains("{@link") {
            out.push_str(line);
            continue;
        }
        let mut rest = line;
        let mut in_code = false;
        while let Some(k) = rest.find(['`', '{']) {
            out.push_str(&rest[..k]);
            rest = &rest[k..];
            if rest.starts_with('`') {
                in_code = !in_code;
                out.push('`');
                rest = &rest[1..];
                continue;
            }
            match (in_code, inline_link(rest)) {
                (false, Some((rendered, after))) => {
                    out.push_str(&rendered);
                    rest = after;
                }
                _ => {
                    out.push('{');
                    rest = &rest[1..];
                }
            }
        }
        out.push_str(rest);
    }
    out
}

/// `{@link …}` at the start of `s`: its rendering and the text after it.
fn inline_link(s: &str) -> Option<(String, &str)> {
    let inner = s
        .strip_prefix("{@linkcode")
        .or_else(|| s.strip_prefix("{@linkplain"))
        .or_else(|| s.strip_prefix("{@link"))?;
    if !inner.starts_with(char::is_whitespace) {
        return None;
    }
    let end = inner.find('}')?;
    let content = inner[..end].trim();
    let (target, label) = match content.split_once('|') {
        Some((t, l)) => (t.trim(), l.trim()),
        None => match content.split_once(char::is_whitespace) {
            Some((t, l)) => (t, l.trim()),
            None => (content, ""),
        },
    };
    let url = target.starts_with("http://") || target.starts_with("https://");
    let rendered = match (url, label.is_empty()) {
        (true, true) => format!("[{target}]({target})"),
        (true, false) => format!("[{label}]({target})"),
        (false, true) => format!("`{target}`"),
        (false, false) => label.to_string(),
    };
    Some((rendered, &inner[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doc of the declaration at the first `export`/`function` in `src`.
    fn doc_of(src: &str) -> Option<DocComment> {
        let lo = src
            .find("export")
            .or_else(|| src.find("function"))
            .expect("a declaration");
        doc_before(src, lo as u32)
    }

    fn body(src: &str) -> Option<String> {
        doc_of(src).map(|d| d.body)
    }

    #[test]
    fn jsdoc_block() {
        let src = "/**\n * Adds two numbers.\n *\n * Twice.\n */\nexport function add() {}\n";
        assert_eq!(body(src).as_deref(), Some("Adds two numbers.\n\nTwice."));
        assert_eq!(
            body("/** One line. */\nexport const X = 1;\n").as_deref(),
            Some("One line.")
        );
        assert_eq!(
            body("/** Same line. */ export const X = 1;\n").as_deref(),
            Some("Same line.")
        );
    }

    #[test]
    fn triple_slash_block() {
        let src = "// not this\n/// First.\n///\n///     code\nexport function f() {}\n";
        assert_eq!(body(src).as_deref(), Some("First.\n\n    code"));
    }

    #[test]
    fn plain_comments_are_not_docs() {
        assert_eq!(body("// Adds.\nexport function f() {}\n"), None);
        assert_eq!(body("/* Adds. */\nexport function f() {}\n"), None);
        assert_eq!(body("//// Adds.\nexport function f() {}\n"), None);
        assert_eq!(body("/**/\nexport function f() {}\n"), None);
    }

    #[test]
    fn plain_comments_between_a_doc_comment_and_its_declaration_are_skipped() {
        // As in TypeScript: line-adjacent plain comments (lint and compiler directives).
        let src = "/** Doc. */\n// eslint-disable-next-line\n// @ts-expect-error\nexport function f() {}\n";
        assert_eq!(body(src).as_deref(), Some("Doc."));
        let src = "/// Doc.\n/* prettier-ignore */\nexport function f() {}\n";
        assert_eq!(body(src).as_deref(), Some("Doc."));
        let src = "class A {\n  /** Count. */\n  // prettier-ignore\n  count: i64 = 0;\n}\n";
        let lo = src.find("count:").unwrap() as u32;
        assert_eq!(
            doc_before(src, lo).map(|d| d.body).as_deref(),
            Some("Count.")
        );
        // A blank line still ends the association, wherever it is.
        assert_eq!(
            body("/** Doc. */\n\n// note\nexport function f() {}\n"),
            None
        );
        assert_eq!(
            body("/** Doc. */\n// note\n\nexport function f() {}\n"),
            None
        );
        // Plain comments alone document nothing; a trailing comment of code is no doc comment.
        assert_eq!(body("// a\n// b\nexport function f() {}\n"), None);
        assert_eq!(
            body("let a = 1; /** a */\n// b\nexport function f() {}\n"),
            None
        );
    }

    #[test]
    fn a_blank_line_ends_the_association() {
        assert_eq!(body("/** Doc. */\n\nexport function f() {}\n"), None);
        assert_eq!(body("/// Doc.\n\nexport function f() {}\n"), None);
        // A blank line inside a `///` run splits it.
        assert_eq!(
            body("/// Section.\n\n/// Doc.\nexport function f() {}\n").as_deref(),
            Some("Doc.")
        );
    }

    #[test]
    fn trailing_comments_of_code_are_not_docs() {
        assert_eq!(body("let a = 1; /** a */\nexport function f() {}\n"), None);
        assert_eq!(body("let a = 1; /// a\nexport function f() {}\n"), None);
    }

    #[test]
    fn comments_inside_template_strings_are_ignored() {
        let src = "const t = `\n/// not a doc\n`;\nexport function f() {}\n";
        assert_eq!(doc_before(src, src.find("export").unwrap() as u32), None);
        let src = "const t = `\n/** not a doc */\n`;\nexport function f() {}\n";
        assert_eq!(doc_before(src, src.find("export").unwrap() as u32), None);
    }

    #[test]
    fn export_and_modifiers() {
        let src = "/** Doc. */\nexport async function f() {}\n";
        assert_eq!(body(src).as_deref(), Some("Doc."));
        let src = "class A {\n  /** Count. */\n  static count: i64 = 0;\n}\n";
        let lo = src.find("static").unwrap() as u32;
        assert_eq!(
            doc_before(src, lo).map(|d| d.body).as_deref(),
            Some("Count.")
        );
    }

    #[test]
    fn params() {
        let doc = parse(
            "/**\n * Joins.\n * @param a - the first\n * @param {string} b the second,\n *   on two lines\n * @param [c=1] - optional\n * @param d\n */",
        );
        assert_eq!(doc.body, "Joins.");
        assert_eq!(
            doc.params,
            [
                ("a".to_string(), "the first".to_string()),
                ("b".to_string(), "the second,\non two lines".to_string()),
                ("c".to_string(), "optional".to_string()),
                ("d".to_string(), String::new()),
            ]
        );
        assert_eq!(doc.param("b"), Some("the second,\non two lines"));
        // A default with brackets of its own.
        let doc = parse("/** @param [opts=[]] - the options\n * @param [m=[[1], [2]]] rows */");
        assert_eq!(doc.param("opts"), Some("the options"));
        assert_eq!(doc.param("m"), Some("rows"));
        assert_eq!(doc.param("z"), None);
    }

    #[test]
    fn returns_throws_deprecated_see() {
        let doc = parse(
            "/**\n * @returns {i64} the sum\n * @throws {RangeError} if negative\n * @throws when closed\n * @deprecated Use {@link sum2} instead.\n * @see {@link https://example.com docs}\n * @see sum3\n */",
        );
        assert_eq!(doc.body, "");
        assert_eq!(doc.returns.as_deref(), Some("the sum"));
        assert_eq!(doc.throws, ["`RangeError`: if negative", "when closed"]);
        assert_eq!(doc.deprecated.as_deref(), Some("Use `sum2` instead."));
        assert_eq!(doc.see, ["[docs](https://example.com)", "sum3"]);
        assert_eq!(parse("/** @return x */").returns.as_deref(), Some("x"));
        assert_eq!(parse("/** @deprecated */").deprecated.as_deref(), Some(""));
    }

    #[test]
    fn examples_are_verbatim() {
        let doc = parse(
            "/**\n * Text.\n * @example\n * const x = f({ a: 1 });\n *   // @param inside the example\n * @example\n * ```ts\n * f();\n * ```\n */",
        );
        assert_eq!(
            doc.examples,
            [
                "const x = f({ a: 1 });\n  // @param inside the example",
                "```ts\nf();\n```"
            ]
        );
        assert!(doc.params.is_empty());
    }

    #[test]
    fn links_and_unknown_tags() {
        let doc = parse("/**\n * See {@link Foo.bar}, {@link Baz the baz} and `{@link no}`.\n * @since 1.0\n */");
        assert_eq!(
            doc.body,
            "See `Foo.bar`, the baz and `{@link no}`.\n\n@since 1.0"
        );
    }

    #[test]
    fn code_fences_keep_their_indentation_and_tags() {
        let doc =
            parse("/**\n * Use:\n *\n * ```ts\n * if (x) {\n *   @param(1);\n * }\n * ```\n */");
        assert_eq!(doc.body, "Use:\n\n```ts\nif (x) {\n  @param(1);\n}\n```");
        // A block without leading stars loses its common indentation.
        let doc = parse("/**\n    First.\n      indented\n */");
        assert_eq!(doc.body, "First.\n  indented");
    }

    #[test]
    fn markdown_rendering() {
        let doc = parse(
            "/**\n * Adds.\n * @param a - one\n *   more\n * @param b - two\n * @returns the sum\n * @throws if it overflows\n * @example\n * add(1, 2);\n * @deprecated\n * @see sub\n */",
        );
        assert_eq!(
            doc.render_markdown(),
            "Adds.\n\n**Parameters**\n\n- `a`: one\n  more\n- `b`: two\n\n**Returns:** the sum\n\n\
             **Throws:** if it overflows\n\n**Example**\n\n```ts\nadd(1, 2);\n```\n\n\
             **Deprecated.**\n\n**See also:** sub"
        );
        assert_eq!(parse("/// Plain.").render_markdown(), "Plain.");
        assert!(parse("/** */").is_empty());
    }

    #[test]
    fn module_docs() {
        let docs = |src: &str| module_doc(src, &velt_syntax::comment_ranges(src)).map(|d| d.body);
        assert_eq!(
            docs("// Module.\n//\n// More.\n\nexport function f() {}\n").as_deref(),
            Some("Module.\n\nMore.")
        );
        assert_eq!(
            docs("/**\n * Module.\n */\n\nexport function f() {}\n").as_deref(),
            Some("Module.")
        );
        assert_eq!(
            docs("#!/usr/bin/env velt\n// Script.\n\nf();\n").as_deref(),
            Some("Script.")
        );
        assert_eq!(
            docs("// Only a comment.\n").as_deref(),
            Some("Only a comment.")
        );
        // Without a blank line the comment documents the declaration.
        assert_eq!(docs("/** f. */\nexport function f() {}\n"), None);
        assert_eq!(docs("import x;\n// late\n\n"), None);
    }
}
