//! Shared helpers for the formatter's integration tests: the corpus of real `.vlt` files and a
//! span/NodeId-insensitive AST comparison, and the formatter's guarantees as one check.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use velt_common::FileId;
use velt_fmt::{count_comments, format_source};

/// Workspace root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `.vlt` file under `tests/golden`, `std` and `fuzz/seeds` (whatever exists), plus the
/// directories listed in `VELT_FMT_EXTRA_CORPUS` (separated like `PATH`), sorted.
pub fn corpus() -> Vec<PathBuf> {
    let mut out = vec![];
    for dir in ["tests/golden", "std", "fuzz/seeds"] {
        collect(&root().join(dir), &mut out);
    }
    if let Some(extra) = std::env::var_os("VELT_FMT_EXTRA_CORPUS") {
        for dir in std::env::split_paths(&extra) {
            collect(&dir, &mut out);
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "vlt") {
            out.push(path);
        }
    }
}

/// Does `src` parse without errors?
pub fn parses(src: &str) -> bool {
    let (_, diags) = velt_syntax::parse_file(FileId(0), src);
    !diags.iter().any(|d| d.is_error())
}

/// The AST dump of `src` with spans and node ids erased, so two sources that differ only in
/// layout compare equal. The text of intrinsic elements and fragments is compared as rendered:
/// neighbouring text and `{" "}` string children are joined (the formatter moves spaces next to
/// tags between the two, like prettier). A component's children must stay exactly as written.
pub fn ast_shape(src: &str) -> String {
    let (module, _) = velt_syntax::parse_file(FileId(0), src);
    let dump = velt_syntax::dump(&module);
    let mut out = String::with_capacity(dump.len());
    let mut skip_number = false;
    for line in dump.lines() {
        let t = line.trim();
        if skip_number {
            skip_number = false;
            if t.trim_end_matches(',').chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
        }
        let is_offset = (t.starts_with("lo: ") || t.starts_with("hi: "))
            && t[4..]
                .trim_end_matches(',')
                .chars()
                .all(|c| c.is_ascii_digit());
        if is_offset {
            continue;
        }
        if t.ends_with("NodeId(") {
            skip_number = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    let lines: Vec<&str> = out.lines().collect();
    let mut joined = jsx_text_joined(&drop_spans(&lines)).join("\n");
    joined.push('\n');
    joined
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `lines` without the `span: Span { … }` blocks (only the file id is left in them).
fn drop_spans<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut out = vec![];
    let mut skip_to: Option<usize> = None;
    for line in lines {
        if let Some(indent) = skip_to {
            if indent_of(line) == indent && line.trim_start().starts_with('}') {
                skip_to = None;
            }
            continue;
        }
        if line.trim_start() == "span: Span {" {
            skip_to = Some(indent_of(line));
            continue;
        }
        out.push(*line);
    }
    out
}

/// The dump with every list of JSX children rewritten so that runs of text and `{"  "}`
/// children become one text child.
fn jsx_text_joined(lines: &[&str]) -> Vec<String> {
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line.to_string());
        i += 1;
        if line.trim_start() != "children: [" {
            continue;
        }
        let indent = indent_of(line);
        let end = (i..lines.len())
            .find(|&j| indent_of(lines[j]) == indent && lines[j].trim_start().starts_with(']'))
            .unwrap_or(lines.len());
        let join = renders_children_only(&out, indent);
        out.extend(joined_children(&lines[i..end], indent + 4, join));
        i = end;
    }
    out
}

/// Are the children of the element whose fields are dumped at `indent` (ending `out`) only
/// rendered: an intrinsic element or a fragment? A component receives them as a prop (one child
/// as itself, several as an array), so its children must stay exactly as written.
fn renders_children_only(out: &[String], indent: usize) -> bool {
    let Some(at) = out
        .iter()
        .rposition(|l| indent_of(l) == indent && l.trim_start().starts_with("name: "))
    else {
        return false;
    };
    let rest: Vec<&str> = out[at..].iter().map(|l| l.trim()).collect();
    match rest.get(1).copied() {
        _ if rest[0] == "name: None," => true,
        Some("Namespaced(") => true,
        Some("Ident(") => rest
            .iter()
            .find_map(|l| l.strip_prefix("name: \""))
            .is_some_and(|tag| {
                tag.starts_with(|c: char| c.is_ascii_lowercase()) || tag.contains('-')
            }),
        _ => false,
    }
}

/// The children (dump lines at `indent`), with text runs joined if `join`.
fn joined_children(lines: &[&str], indent: usize, join: bool) -> Vec<String> {
    let pad = " ".repeat(indent);
    let flush = |out: &mut Vec<String>, text: &mut Option<String>| {
        if let Some(value) = text.take() {
            out.push(format!("{pad}Text {{"));
            out.push(format!("{pad}    value: \"{value}\","));
            out.push(format!("{pad}}},"));
        }
    };
    let mut out = vec![];
    let mut text: Option<String> = None;
    let mut start = 0;
    while start < lines.len() {
        let end = if lines[start].trim_end().ends_with(',') {
            start + 1
        } else {
            (start + 1..lines.len())
                .find(|&j| indent_of(lines[j]) == indent)
                .map_or(lines.len(), |j| j + 1)
        };
        let item = &lines[start..end];
        start = end;
        if let Some(value) = text_value(item).filter(|_| join) {
            text.get_or_insert_with(String::new).push_str(&value);
            continue;
        }
        flush(&mut out, &mut text);
        out.extend(jsx_text_joined(item));
    }
    flush(&mut out, &mut text);
    out
}

/// The (escaped) text of a text child or of a `{"  "}` child.
fn text_value(item: &[&str]) -> Option<String> {
    let flat: String = item.iter().map(|l| l.trim()).collect();
    if let Some(rest) = flat.strip_prefix("Text {value: \"") {
        return rest.strip_suffix("\",},").map(str::to_string);
    }
    let value = flat
        .strip_prefix("Expr {expr: Some(Expr {id: NodeId(),kind: Lit(Str(\"")?
        .strip_suffix("\",),),},),},")?;
    value.chars().all(|c| c == ' ').then(|| value.to_string())
}

/// First differing line of two texts, for readable assertion messages.
pub fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!("line {}:\n  - {x}\n  + {y}", i + 1);
        }
    }
    format!(
        "lengths differ: {} vs {} lines",
        a.lines().count(),
        b.lines().count()
    )
}

/// The formatter's guarantees for one parseable source.
pub fn check(src: &str) -> Result<(), String> {
    let once = format_source(src).map_err(|d| format!("refused: {d:?}"))?;
    if !parses(&once) {
        return Err(format!("output does not parse:\n{once}"));
    }
    let (before, after) = (ast_shape(src), ast_shape(&once));
    if before != after {
        return Err(format!(
            "AST changed: {}",
            first_difference(&before, &after)
        ));
    }
    if count_comments(src) != count_comments(&once) {
        return Err(format!("comments lost:\n{once}"));
    }
    let twice = format_source(&once).map_err(|d| format!("output refused: {d:?}"))?;
    if once != twice {
        return Err(format!(
            "not idempotent: {}",
            first_difference(&once, &twice)
        ));
    }
    Ok(())
}
