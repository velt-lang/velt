//! Which files are Velt source modules: `.vlt`, and TypeScript's `.ts` and `.tsx` (so a folder
//! can be shared with a TypeScript project). Declaration files (`.d.ts`) are not modules. Every
//! tool that enumerates source files (the loader's relative imports, `velt check` in a package,
//! `velt test`, `velt fmt`, `velt doc`, the language server) uses these rules.

use std::path::Path;

/// Source file extensions, in the order a relative import tries them (`./x` → `x.vlt`, `x.ts`,
/// `x.tsx`).
pub const SOURCE_EXTENSIONS: [&str; 3] = ["vlt", "ts", "tsx"];

/// Whether a file named `name` is a source module (`a.vlt`, `a.ts`, `a.tsx`; not `a.d.ts`).
pub fn is_source_name(name: &str) -> bool {
    strip_source_extension(name).is_some()
}

/// Whether `path` names a source module ([`is_source_name`] of its file name).
pub fn is_source_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_source_name)
}

/// `name` without its source extension (`a.test.ts` → `a.test`), or `None` if it is not a
/// source module.
pub fn strip_source_extension(name: &str) -> Option<&str> {
    if name.ends_with(".d.ts") {
        return None;
    }
    SOURCE_EXTENSIONS.iter().find_map(|ext| {
        name.strip_suffix(ext)
            .and_then(|s| s.strip_suffix('.'))
            .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
    })
}

/// Whether `path` is a TypeScript file without JSX (`.ts`): JSX is allowed only in `.tsx` (and
/// `.vlt`) files, as in TypeScript.
pub fn is_plain_ts(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "ts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names() {
        for name in ["a.vlt", "a.ts", "a.tsx", "a.test.ts", "dir/x.tsx"] {
            assert!(is_source_name(name), "{name}");
        }
        for name in ["a.d.ts", "a.js", "a.vltx", "ts", ".ts", "a.json", "a.mts"] {
            assert!(!is_source_name(name), "{name}");
        }
        assert_eq!(strip_source_extension("a.test.ts"), Some("a.test"));
        assert_eq!(strip_source_extension("dir/x.tsx"), Some("dir/x"));
        assert!(is_plain_ts(Path::new("x.ts")));
        assert!(!is_plain_ts(Path::new("x.tsx")));
    }
}
