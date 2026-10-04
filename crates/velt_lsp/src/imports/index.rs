//! The modules a loader lists in [`crate::ProgramLoader::module_index`]: the public modules of a
//! standard library root and the modules of a dependency. Shared by `veltc`'s loader and the
//! tests' loader, so both list the same modules.
//!
//! A std module is internal (not offered) when its header comment says so (`std/url/encode
//! (std-internal): …`, `Internal: import the public names from "velt:redis"`), or when it sits
//! in the folder of a public module of the same name (`url/*.vlt` next to `url.vlt`); a JSX
//! runtime (`jsx/jsx-runtime`) is public wherever it is. The prelude is imported implicitly and
//! never listed.

use std::path::{Path, PathBuf};

use crate::{ModuleEntry, ModuleKind};

/// At most this many modules are listed per std root or dependency.
const MAX_MODULES: usize = 500;
/// A module description is cut at this many characters.
const MAX_DOC_CHARS: usize = 160;

/// The public modules of the standard library at `std_root`, sorted by specifier.
pub fn std_module_entries(std_root: &Path) -> Vec<ModuleEntry> {
    let mut files = vec![];
    // std modules are `.vlt` files; its `package.vlt` is the module `velt:package`.
    source_files(std_root, &|name| name.ends_with(".vlt"), &mut files);
    let mut out: Vec<ModuleEntry> = files
        .into_iter()
        .filter_map(|path| {
            let rel = module_rel(std_root, &path)?;
            if rel == "prelude" || rel.starts_with("prelude/") || !is_std_rel(&rel) {
                return None;
            }
            let header = header(&path);
            if is_internal(std_root, &rel, &header) {
                return None;
            }
            Some(ModuleEntry {
                doc: describe(&header),
                spec: format!("velt:{rel}"),
                path,
                kind: ModuleKind::Std,
            })
        })
        .collect();
    out.sort_by(|a, b| a.spec.cmp(&b.spec));
    out
}

/// The modules of dependency `name` whose package root is `root`: its default module (`name`,
/// `src/lib.vlt`) and the other modules of its `src/` (`name/sub`), sorted by specifier.
pub fn package_module_entries(name: &str, root: &Path) -> Vec<ModuleEntry> {
    let src = root.join(vpm::manifest::SRC_DIR);
    let mut files = vec![];
    source_files(&src, &vpm::sources::walk_keeps, &mut files);
    let mut out: Vec<ModuleEntry> = files
        .into_iter()
        .filter_map(|path| {
            let rel = module_rel(&src, &path)?;
            let spec = if rel == "lib" {
                name.to_string()
            } else {
                format!("{name}/{rel}")
            };
            Some(ModuleEntry {
                spec,
                path,
                kind: ModuleKind::Dependency,
                doc: String::new(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.spec.cmp(&b.spec));
    out.dedup_by(|a, b| a.spec == b.spec);
    out
}

/// The files below `dir` whose name `keep` takes, in the directories `velt build` walks, at most
/// [`MAX_MODULES`].
fn source_files(dir: &Path, keep: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if out.len() >= MAX_MODULES {
            return;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if vpm::sources::walks_into(&path) {
                source_files(&path, keep, out);
            }
        } else if keep(name) {
            out.push(path);
        }
    }
}

/// The module path of `file` below `base` (`/`-separated, no extension or final `/index`).
fn module_rel(base: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(base).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let rel = parts?.join("/");
    let rel = vpm::sources::strip_source_extension(&rel)?;
    let rel = rel.strip_suffix("/index").unwrap_or(rel);
    (rel != "index").then(|| rel.to_string())
}

/// A path a `velt:` specifier can spell: `/`-separated segments of `[a-z0-9_-]`.
fn is_std_rel(rel: &str) -> bool {
    rel.split('/').all(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    })
}

fn is_internal(std_root: &Path, rel: &str, header: &str) -> bool {
    if says_internal(header) {
        return true;
    }
    if rel.ends_with("jsx-runtime") {
        return false;
    }
    // In the folder of a public module of the same name (`url/encode` next to `url.vlt`).
    let segments: Vec<&str> = rel.split('/').collect();
    (1..segments.len()).any(|n| {
        std_root
            .join(format!("{}.vlt", segments[..n].join("/")))
            .is_file()
    })
}

/// Whether `header` calls the module internal (the word, not `docs/internals/…`).
fn says_internal(header: &str) -> bool {
    let lower = header.to_ascii_lowercase();
    lower
        .match_indices("internal")
        .any(|(i, word)| !lower[i + word.len()..].starts_with(|c: char| c.is_ascii_alphabetic()))
}

/// The first paragraph of the `//` comment a file starts with, as one line.
fn header(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut out = String::new();
    for line in text.lines() {
        let Some(comment) = line.trim_start().strip_prefix("//") else {
            break;
        };
        let comment = comment.trim_start_matches('/').trim();
        if comment.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(comment);
    }
    out
}

/// A module's description from its header: without the leading `std/x: ` and cut to
/// [`MAX_DOC_CHARS`].
fn describe(header: &str) -> String {
    let text = match header.split_once(": ") {
        Some((name, rest)) if name.starts_with("std/") && !name.contains(' ') => rest,
        _ => header,
    };
    if text.chars().count() <= MAX_DOC_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_DOC_CHARS).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn lists_public_std_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "fs.vlt",
            "// std/fs: file system access (docs/internals/x.md).\n//\n// More.\n",
        );
        write(root, "url.vlt", "// std/url: URLs.\n");
        write(root, "url/encode.vlt", "// std/url/encode: helpers.\n");
        write(
            root,
            "net_bytes.vlt",
            "// std/net_bytes (std-internal): bytes.\n",
        );
        write(
            root,
            "collections/set.vlt",
            "// std/collections/set: `Set<T>`.\n",
        );
        write(root, "jsx.vlt", "// std/jsx: rendering.\n");
        write(
            root,
            "jsx/jsx-runtime.vlt",
            "// std/jsx/jsx-runtime: runtime.\n",
        );
        write(root, "package.vlt", "// std/package: manifest types.\n");
        write(root, "prelude/array.vlt", "// arrays\n");
        write(root, "README.md", "# std\n");
        write(root, "Bad.vlt", "");
        let entries = std_module_entries(root);
        let specs: Vec<&str> = entries.iter().map(|e| e.spec.as_str()).collect();
        assert_eq!(
            specs,
            [
                "velt:collections/set",
                "velt:fs",
                "velt:jsx",
                "velt:jsx/jsx-runtime",
                "velt:package",
                "velt:url"
            ]
        );
        assert_eq!(entries[1].doc, "file system access (docs/internals/x.md).");
        assert_eq!(entries[1].path, root.join("fs.vlt"));
    }

    #[test]
    fn lists_dependency_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib.vlt", "");
        write(root, "src/parse.ts", "");
        write(root, "src/shapes/index.vlt", "");
        write(root, "src/types.d.ts", "");
        let specs: Vec<String> = package_module_entries("json", root)
            .into_iter()
            .map(|e| e.spec)
            .collect();
        assert_eq!(specs, ["json", "json/parse", "json/shapes"]);
    }
}
