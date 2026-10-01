//! Lexical path arithmetic (no filesystem access): normalizing `.`/`..` and expressing one path
//! relative to another. Used for lockfile path sources and by the module loader.

use std::path::{Component, Path, PathBuf};

/// Lexically normalize `.` / `..` components. Leading `..` that cannot be popped are kept.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Make `p` absolute against the current directory (lexically normalized).
pub fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        normalize(&cwd.join(p))
    }
}

/// `path` relative to `base` as a `/`-separated string (both normalized, same anchor), e.g.
/// `relative("/a/b/c", "/a/x") == "../b/c"`. Returns `"."` for equal paths.
pub fn relative(path: &Path, base: &Path) -> String {
    let path: Vec<_> = normalize(path)
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let base: Vec<_> = normalize(base)
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); base.len() - common];
    parts.extend(
        path[common..]
            .iter()
            .map(|c| c.to_string_lossy().into_owned()),
    );
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize(Path::new("a/./b/../c")), Path::new("a").join("c"));
        assert_eq!(normalize(Path::new("../a")), Path::new("..").join("a"));
        assert_eq!(normalize(Path::new("/../a")), Path::new("/a"));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b/c"), Path::new("/a/x")), "../b/c");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
        assert_eq!(relative(Path::new("/a/b/c/d"), Path::new("/a/b")), "c/d");
        assert_eq!(
            relative(Path::new("src/x.vlt"), Path::new("")),
            "src/x.vlt"
        );
    }
}
