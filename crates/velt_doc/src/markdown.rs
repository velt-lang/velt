//! A small Markdown → HTML renderer for doc comments and the docs site: ATX headings (with
//! anchor ids), paragraphs, fenced code blocks, bullet and numbered lists (nested by
//! indentation), block quotes, tables, and inline code, links, `**bold**` and `*italic*`.
//! Raw HTML is escaped. Links to `*.md` files are rewritten to `*.html` so the repository's docs
//! link to each other on the site too.

/// Render `text` to HTML.
pub fn to_html(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        i = block(&lines, i, &mut out);
    }
    out
}

/// Render the block starting at line `i`; returns the index after it.
fn block(lines: &[&str], i: usize, out: &mut String) -> usize {
    let line = lines[i];
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return i + 1;
    }
    if let Some(fence) = trimmed.strip_prefix("```") {
        return code_block(lines, i, fence.trim(), out);
    }
    if let Some((level, title)) = heading(trimmed) {
        let id = slug(title);
        out.push_str(&format!(
            "<h{level} id=\"{id}\">{}</h{level}>\n",
            inline(title)
        ));
        return i + 1;
    }
    if trimmed.starts_with('|') && lines.get(i + 1).is_some_and(|l| is_table_rule(l)) {
        return table(lines, i, out);
    }
    if trimmed.starts_with('>') {
        return quote(lines, i, out);
    }
    if list_marker(line).is_some() {
        return list(lines, i, indent_of(line), out);
    }
    paragraph(lines, i, out)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let rest = line.get(level..)?;
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

/// An anchor id: lowercase letters, digits and dashes.
pub fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            s.extend(c.to_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !s.ends_with('-') {
            s.push('-');
        }
    }
    s.trim_matches('-').to_string()
}

fn code_block(lines: &[&str], i: usize, lang: &str, out: &mut String) -> usize {
    let indent = indent_of(lines[i]);
    let mut j = i + 1;
    let mut code = String::new();
    while j < lines.len() && !lines[j].trim_start().starts_with("```") {
        let l = lines[j];
        code.push_str(l.get(indent.min(indent_of(l))..).unwrap_or(""));
        code.push('\n');
        j += 1;
    }
    let class = if lang.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", escape(lang))
    };
    out.push_str(&format!(
        "<pre><code{class}>{}</code></pre>\n",
        escape(&code)
    ));
    j + 1
}

fn is_table_rule(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn cells(line: &str) -> Vec<&str> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    split_cells(t)
}

/// Split on `|` outside of inline code spans.
fn split_cells(t: &str) -> Vec<&str> {
    let mut cells = vec![];
    let (mut start, mut in_code) = (0, false);
    for (k, c) in t.char_indices() {
        match c {
            '`' => in_code = !in_code,
            '|' if !in_code => {
                cells.push(t[start..k].trim());
                start = k + 1;
            }
            _ => {}
        }
    }
    cells.push(t[start..].trim());
    cells
}

fn table(lines: &[&str], i: usize, out: &mut String) -> usize {
    out.push_str("<table>\n<thead><tr>");
    for c in cells(lines[i]) {
        out.push_str(&format!("<th>{}</th>", inline(c)));
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    let mut j = i + 2;
    while j < lines.len() && lines[j].trim_start().starts_with('|') {
        out.push_str("<tr>");
        for c in cells(lines[j]) {
            out.push_str(&format!("<td>{}</td>", inline(c)));
        }
        out.push_str("</tr>\n");
        j += 1;
    }
    out.push_str("</tbody>\n</table>\n");
    j
}

fn quote(lines: &[&str], i: usize, out: &mut String) -> usize {
    let mut j = i;
    let mut inner = vec![];
    while j < lines.len() && lines[j].trim_start().starts_with('>') {
        let t = lines[j].trim_start().trim_start_matches('>');
        inner.push(t.strip_prefix(' ').unwrap_or(t));
        j += 1;
    }
    out.push_str(&format!(
        "<blockquote>\n{}</blockquote>\n",
        to_html(&inner.join("\n"))
    ));
    j
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `(ordered, content)` if the line starts a list item.
fn list_marker(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((false, rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let rest = t.get(digits..)?;
    (digits > 0 && rest.starts_with(". ")).then(|| (true, &rest[2..]))
}

/// A list at `indent`: items, their continuation lines, and nested lists.
fn list(lines: &[&str], i: usize, indent: usize, out: &mut String) -> usize {
    let ordered = list_marker(lines[i]).is_some_and(|(o, _)| o);
    let tag = if ordered { "ol" } else { "ul" };
    out.push_str(&format!("<{tag}>\n"));
    let mut j = i;
    while j < lines.len() {
        let line = lines[j];
        let Some((_, content)) = list_marker(line).filter(|_| indent_of(line) == indent) else {
            break;
        };
        let mut text = content.to_string();
        j += 1;
        let mut nested = String::new();
        while j < lines.len() {
            let l = lines[j];
            if l.trim().is_empty() {
                let continues = lines.get(j + 1).is_some_and(|n| indent_of(n) > indent);
                if !continues {
                    break;
                }
                j += 1;
            } else if indent_of(l) > indent && list_marker(l).is_some() {
                j = list(lines, j, indent_of(l), &mut nested);
            } else if indent_of(l) > indent && l.trim_start().starts_with("```") {
                j = code_block(lines, j, l.trim_start()[3..].trim(), &mut nested);
            } else if indent_of(l) > indent || (list_marker(l).is_none() && !starts_block(l)) {
                text.push(' ');
                text.push_str(l.trim());
                j += 1;
            } else {
                break;
            }
        }
        out.push_str(&format!("<li>{}{nested}</li>\n", inline(&text)));
    }
    out.push_str(&format!("</{tag}>\n"));
    j
}

fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with('#') || t.starts_with('|') || t.starts_with('>')
}

fn paragraph(lines: &[&str], i: usize, out: &mut String) -> usize {
    let mut j = i;
    let mut text = vec![];
    while j < lines.len() {
        let l = lines[j];
        if l.trim().is_empty() || (j > i && (starts_block(l) || list_marker(l).is_some())) {
            break;
        }
        text.push(l.trim());
        j += 1;
    }
    out.push_str(&format!("<p>{}</p>\n", inline(&text.join(" "))));
    j
}

/// Escape `&`, `<`, `>` and `"`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// Inline markup of one paragraph / cell / heading.
pub fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while !rest.is_empty() {
        let Some(k) = rest.find(['`', '[', '*']) else {
            out.push_str(&escape(rest));
            break;
        };
        out.push_str(&escape(&rest[..k]));
        rest = &rest[k..];
        rest = match inline_span(rest, &mut out) {
            Some(after) => after,
            None => {
                out.push_str(&escape(&rest[..1]));
                &rest[1..]
            }
        };
    }
    out
}

/// Render the span at the start of `s` (code, link, bold, italic); the rest after it.
fn inline_span<'a>(s: &'a str, out: &mut String) -> Option<&'a str> {
    if s.starts_with('`') {
        // A run of n backticks ends at the next run of exactly n (CommonMark code spans).
        let n = s.bytes().take_while(|&b| b == b'`').count();
        let fence = &s[..n];
        let body = &s[n..];
        let end = body.find(fence)?;
        let code = body[..end]
            .strip_prefix(' ')
            .and_then(|c| c.strip_suffix(' '));
        let code = code.filter(|_| n > 1).unwrap_or(&body[..end]);
        out.push_str(&format!("<code>{}</code>", escape(code)));
        return Some(&body[end + n..]);
    }
    if let Some(body) = s.strip_prefix('[') {
        let close = body.find("](")?;
        let end = body[close..].find(')')? + close;
        let (label, url) = (&body[..close], &body[close + 2..end]);
        if is_safe_link(url) {
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escape(&md_link(url)),
                inline(label)
            ));
        } else {
            // Doc comments come from third-party packages too; a `javascript:` link would
            // run their script in the reader's browser, so unsafe targets lose the link.
            out.push_str(&inline(label));
        }
        return Some(&body[end + 1..]);
    }
    if let Some(body) = s.strip_prefix("**") {
        let end = body.find("**")?;
        out.push_str(&format!("<strong>{}</strong>", inline(&body[..end])));
        return Some(&body[end + 2..]);
    }
    let body = s.strip_prefix('*')?;
    if body.starts_with(' ') {
        return None;
    }
    let end = body.find('*')?;
    out.push_str(&format!("<em>{}</em>", inline(&body[..end])));
    Some(&body[end + 1..])
}

/// Whether `url` is relative, an anchor, or uses an `http`/`https`/`mailto` scheme.
fn is_safe_link(url: &str) -> bool {
    // Browsers ignore whitespace and control characters inside a scheme (`java\tscript:`),
    // so they are dropped before the scheme is read.
    let normalized: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    match normalized.find([':', '/', '?', '#']) {
        Some(k) if normalized[k..].starts_with(':') => {
            let scheme = normalized[..k].to_ascii_lowercase();
            matches!(scheme.as_str(), "http" | "https" | "mailto")
        }
        _ => true,
    }
}

/// `guide.md#x` → `guide.html#x` for relative links.
fn md_link(url: &str) -> String {
    if url.contains("://") {
        return url.to_string();
    }
    let (path, anchor) = url
        .split_once('#')
        .map_or((url, None), |(p, a)| (p, Some(a)));
    let path = match path.strip_suffix(".md") {
        Some(stem) => format!("{stem}.html"),
        None => path.to_string(),
    };
    match anchor {
        Some(a) => format!("{path}#{a}"),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let html = to_html(
            "# Title here\n\nSome *text* with `a<b` and [link](guide.md#x).\nmore\n\n```ts\nlet x = 1;\n```\n\n- one\n  cont\n  - nested\n- two\n\n1. first\n2. second\n\n> quoted\n\n| a | b |\n|---|---|\n| `x|y` | **z** |\n",
        );
        for needle in [
            "<h1 id=\"title-here\">Title here</h1>",
            "<p>Some <em>text</em> with <code>a&lt;b</code> and <a href=\"guide.html#x\">link</a>. more</p>",
            "<pre><code class=\"language-ts\">let x = 1;\n</code></pre>",
            "<li>one cont<ul>\n<li>nested</li>\n</ul>\n</li>",
            "<li>two</li>",
            "<ol>\n<li>first</li>\n<li>second</li>\n</ol>",
            "<blockquote>\n<p>quoted</p>\n</blockquote>",
            "<th>a</th><th>b</th>",
            "<td><code>x|y</code></td><td><strong>z</strong></td>",
        ] {
            assert!(html.contains(needle), "missing {needle} in\n{html}");
        }
    }

    #[test]
    fn inline_edge_cases() {
        assert_eq!(inline("a * b"), "a * b");
        assert_eq!(inline("`` `a ${x}` `` b"), "<code>`a ${x}`</code> b");
        assert_eq!(inline("x [y] z"), "x [y] z");
        assert_eq!(inline("<script>"), "&lt;script&gt;");
        assert_eq!(slug("Hot reload: `velt dev`"), "hot-reload-velt-dev");
        assert_eq!(md_link("https://x.md"), "https://x.md");
    }

    #[test]
    fn only_safe_link_schemes() {
        for (url, href) in [
            ("https://velt.dev", "https://velt.dev"),
            ("HTTP://velt.dev", "HTTP://velt.dev"),
            ("mailto:a@b.c", "mailto:a@b.c"),
            ("guide.md", "guide.html"),
            ("../api/x.html?q=a:b", "../api/x.html?q=a:b"),
            ("#anchor", "#anchor"),
        ] {
            assert_eq!(
                inline(&format!("[x]({url})")),
                format!("<a href=\"{href}\">x</a>")
            );
        }
        // Each target stops before `)`, so `alert(1` stands for `alert(1)`.
        for url in [
            "javascript:alert(1",
            " JavaScript:alert(1",
            "java\tscript:alert(1",
            "\u{1}javascript:alert(1",
            "data:text/html,x",
            "vbscript:msgbox",
        ] {
            assert_eq!(inline(&format!("[*click*]({url})")), "<em>click</em>");
        }
    }
}
