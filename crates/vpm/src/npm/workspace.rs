//! Workspaces: the package directories a `pnpm-workspace.yaml` (`packages:`) or a root
//! `package.json` (`"workspaces"`, as npm and Yarn write it) lists by glob, so a package of
//! the workspace imports another by name before (or without) `pnpm install` linking it into
//! `node_modules`.

use std::path::{Path, PathBuf};

use super::json::Json;

/// File name of a pnpm workspace's manifest.
pub const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";

/// The workspace patterns of directory `dir`: its `pnpm-workspace.yaml`'s `packages`, else its
/// `package.json`'s `workspaces`; `None` when it has neither.
pub fn patterns(dir: &Path) -> Option<Vec<String>> {
    if let Ok(text) = std::fs::read_to_string(dir.join(PNPM_WORKSPACE)) {
        return Some(pnpm_packages(&text));
    }
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json = Json::parse(&text).ok()?;
    let list = match json.get("workspaces")? {
        Json::Arr(items) => items,
        obj @ Json::Obj(_) => match obj.get("packages")? {
            Json::Arr(items) => items,
            _ => return None,
        },
        _ => return None,
    };
    Some(
        list.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

/// The `packages:` list of a `pnpm-workspace.yaml`: its `- item` lines (quotes removed, `#`
/// comments dropped), read without a YAML library since that is all pnpm reads from it here.
fn pnpm_packages(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut inside = false;
    for line in text.lines() {
        let content = strip_comment(line);
        if content.trim().is_empty() {
            continue;
        }
        let indented = content.starts_with(' ') || content.starts_with('\t');
        if !indented && !content.trim_start().starts_with('-') {
            inside = content.trim_end() == "packages:";
            if let Some(inline) = content.trim().strip_prefix("packages:") {
                // `packages: ["a/*", "b"]` (flow style).
                let inline = inline.trim();
                if let Some(list) = inline.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    out.extend(list.split(',').map(unquote).filter(|s| !s.is_empty()));
                }
            }
            continue;
        }
        if inside {
            if let Some(item) = content.trim().strip_prefix('-') {
                let item = unquote(item);
                if !item.is_empty() {
                    out.push(item);
                }
            }
        }
    }
    out
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '#') if i == 0 || line[..i].ends_with(' ') => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

/// The directories below `root` that `patterns` select (each holding a `package.json`): `*`
/// matches one directory name, `**` any number of them, and a pattern starting with `!`
/// excludes what it matches. `node_modules` and hidden directories are never entered.
pub fn package_dirs(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for pattern in patterns.iter().filter(|p| !p.starts_with('!')) {
        let parts = split(pattern);
        expand(root, &parts, &mut out);
    }
    for pattern in patterns.iter().filter_map(|p| p.strip_prefix('!')) {
        let parts = split(pattern);
        out.retain(|dir| {
            let rel = crate::relpath::relative(dir, root);
            let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            !glob_match(&parts, &segs)
        });
    }
    out.retain(|d| d.join("package.json").is_file());
    out
}

fn split(pattern: &str) -> Vec<&str> {
    let p = pattern.strip_prefix("./").unwrap_or(pattern);
    p.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

fn expand(dir: &Path, parts: &[&str], out: &mut Vec<PathBuf>) {
    let Some((first, rest)) = parts.split_first() else {
        if !out.iter().any(|d| d == dir) {
            out.push(dir.to_path_buf());
        }
        return;
    };
    if *first == "**" {
        expand(dir, rest, out);
        for child in children(dir) {
            expand(&child, parts, out);
        }
        return;
    }
    if !first.contains('*') {
        if *first == ".." {
            return;
        }
        let child = dir.join(first);
        if child.is_dir() {
            expand(&child, rest, out);
        }
        return;
    }
    for child in children(dir) {
        let name = child.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if segment_match(first, name) {
            expand(&child, rest, out);
        }
    }
}

/// The subdirectories of `dir` a workspace glob may enter, sorted by name.
fn children(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            p.is_dir() && name != "node_modules" && !name.starts_with('.')
        })
        .collect();
    dirs.sort();
    dirs
}

fn glob_match(parts: &[&str], segs: &[&str]) -> bool {
    match parts.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| glob_match(rest, &segs[i..])),
        Some((p, rest)) => segs
            .split_first()
            .is_some_and(|(s, more)| segment_match(p, s) && glob_match(rest, more)),
    }
}

/// Whether directory name `name` matches one glob segment (`*` any run of characters).
fn segment_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => {
            name.len() >= pre.len() + post.len()
                && name.starts_with(pre)
                && segment_match_rest(post, &name[pre.len()..])
        }
    }
}

fn segment_match_rest(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => name.ends_with(pattern),
        Some((pre, post)) => (0..=name.len())
            .filter(|&i| name.is_char_boundary(i))
            .any(|i| {
                name[i..].starts_with(pre) && segment_match_rest(post, &name[i + pre.len()..])
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pnpm_workspace_yaml() {
        let text = "packages:\n  - packages/*\n  - 'examples/**' # comment\n  - \"!**/test/**\"\n\nonlyBuiltDependencies:\n  - esbuild\n";
        assert_eq!(
            pnpm_packages(text),
            ["packages/*", "examples/**", "!**/test/**"]
        );
        assert_eq!(pnpm_packages("packages: [a/*, 'b']\n"), ["a/*", "b"]);
    }

    #[test]
    fn expands_globs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in [
            "packages/a",
            "packages/b",
            "packages/no-manifest",
            "examples/x/deep",
            "examples/test/t",
            "node_modules/n",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            if dir != "packages/no-manifest" {
                std::fs::write(root.join(dir).join("package.json"), "{}").unwrap();
            }
        }
        let dirs = package_dirs(
            root,
            &[
                "packages/*".into(),
                "examples/**".into(),
                "!examples/test/**".into(),
            ],
        );
        let rel: Vec<String> = dirs
            .iter()
            .map(|d| crate::relpath::relative(d, root))
            .collect();
        assert_eq!(rel, ["packages/a", "packages/b", "examples/x/deep"]);
        assert!(segment_match("a*c", "abbc"));
        assert!(!segment_match("a*c", "abd"));
    }
}
