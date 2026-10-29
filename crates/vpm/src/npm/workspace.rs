//! Workspaces: the package directories a `pnpm-workspace.yaml` (`packages:`) or a root
//! `package.json` (`"workspaces"`, as npm and Yarn write it) lists by glob, so a package of
//! the workspace imports another by name before (or without) `pnpm install` linking it into
//! `node_modules`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::json::Json;

/// File name of a pnpm workspace's manifest.
pub const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";

/// The workspace patterns of directory `dir`: its `pnpm-workspace.yaml`'s `packages`, else its
/// `package.json`'s `workspaces`; `None` when it has neither.
pub fn patterns(dir: &Path) -> Option<Vec<String>> {
    if let Ok(text) = crate::probe::read_to_string(&dir.join(PNPM_WORKSPACE)) {
        return Some(pnpm_packages(&text));
    }
    let text = crate::probe::read_to_string(&dir.join("package.json")).ok()?;
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

/// The most directories one [`package_dirs`] call lists: a workspace glob over a huge tree
/// stops there instead of taking minutes, and [`Listing::cut`] says so.
pub const MAX_DIRS: usize = 10_000;

/// The package directories of a workspace ([`package_dirs`]).
#[derive(Debug, Default)]
pub struct Listing {
    /// The directories, each holding a `package.json`.
    pub dirs: Vec<PathBuf>,
    /// The pattern whose walk stopped at the directory limit, if one did: packages past it
    /// are missing from [`Listing::dirs`].
    pub cut: Option<String>,
}

/// The directories below `root` that `patterns` select (each holding a `package.json`): `*`
/// matches one directory name, `**` any number of them, and a pattern starting with `!`
/// excludes what it matches. `node_modules` and hidden directories are never entered, `**` does
/// not enter symlinked directories (a link back up would never end), and at most [`MAX_DIRS`]
/// directories are listed.
pub fn package_dirs(root: &Path, patterns: &[String]) -> Listing {
    package_dirs_limited(root, patterns, MAX_DIRS)
}

/// [`package_dirs`], listing at most `max_dirs` directories.
pub fn package_dirs_limited(root: &Path, patterns: &[String], max_dirs: usize) -> Listing {
    let mut walk = Walk {
        out: vec![],
        seen: HashSet::new(),
        listed: 0,
        max_dirs,
        cut: false,
    };
    let mut cut = None;
    for pattern in patterns.iter().filter(|p| !p.starts_with('!')) {
        let parts = split(pattern);
        walk.expand(root, &parts);
        if walk.cut && cut.is_none() {
            cut = Some(pattern.clone());
        }
    }
    let mut out = walk.out;
    for pattern in patterns.iter().filter_map(|p| p.strip_prefix('!')) {
        let parts = split(pattern);
        out.retain(|dir| {
            let rel = crate::relpath::relative(dir, root);
            let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            !glob_match(&parts, &segs)
        });
    }
    out.retain(|d| crate::probe::is_file(&d.join("package.json")));
    Listing { dirs: out, cut }
}

fn split(pattern: &str) -> Vec<&str> {
    let p = pattern.strip_prefix("./").unwrap_or(pattern);
    p.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

struct Walk {
    out: Vec<PathBuf>,
    /// The directories in `out`.
    seen: HashSet<PathBuf>,
    /// Directories listed so far (at most `max_dirs`).
    listed: usize,
    max_dirs: usize,
    /// Whether a directory was left unlisted for the limit.
    cut: bool,
}

impl Walk {
    fn expand(&mut self, dir: &Path, parts: &[&str]) {
        let Some((first, rest)) = parts.split_first() else {
            if self.seen.insert(dir.to_path_buf()) {
                self.out.push(dir.to_path_buf());
            }
            return;
        };
        if *first == "**" {
            self.expand(dir, rest);
            for child in self.children(dir, false) {
                self.expand(&child, parts);
            }
            return;
        }
        if !first.contains('*') {
            if *first == ".." {
                return;
            }
            let child = dir.join(first);
            if crate::probe::is_dir(&child) {
                self.expand(&child, rest);
            }
            return;
        }
        for child in self.children(dir, true) {
            let name = child.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if segment_match(first, name) {
                self.expand(&child, rest);
            }
        }
    }

    /// The subdirectories of `dir` a workspace glob may enter, sorted by name; symlinked ones
    /// only with `links`. None once `max_dirs` directories have been listed.
    fn children(&mut self, dir: &Path, links: bool) -> Vec<PathBuf> {
        if self.listed >= self.max_dirs {
            self.cut = true;
            return vec![];
        }
        self.listed += 1;
        let Ok(entries) = crate::probe::read_dir(dir) else {
            return vec![];
        };
        let mut dirs: Vec<PathBuf> = entries
            .into_iter()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_str().unwrap_or("");
                if name == "node_modules" || name.starts_with('.') {
                    return false;
                }
                match e.file_type() {
                    Ok(t) if t.is_symlink() => links && crate::probe::is_dir(&e.path()),
                    Ok(t) => t.is_dir(),
                    Err(_) => false,
                }
            })
            .map(|e| e.path())
            .collect();
        dirs.sort();
        dirs
    }
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
            .dirs
            .iter()
            .map(|d| crate::relpath::relative(d, root))
            .collect();
        assert_eq!(rel, ["packages/a", "packages/b", "examples/x/deep"]);
        assert!(segment_match("a*c", "abbc"));
        assert!(!segment_match("a*c", "abd"));
    }

    #[test]
    fn a_cut_walk_says_so() {
        // #1091 re-check: a walk stopped at the limit used to end silently.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in ["a/1", "a/2", "a/3", "zzz/target"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("zzz/target/package.json"), "{}").unwrap();
        let all = package_dirs_limited(root, &["**".into()], 100);
        assert_eq!(all.dirs, [root.join("zzz/target")]);
        assert_eq!(all.cut, None);
        let cut = package_dirs_limited(root, &["**".into()], 3);
        assert!(cut.dirs.is_empty());
        assert_eq!(cut.cut.as_deref(), Some("**"));
    }
}
