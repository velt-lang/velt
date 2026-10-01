//! `[paths]` import aliases of a package (CONTRACT: docs/internals/contracts/velt_toml.md):
//!
//! ```toml
//! [paths]
//! "@app/*" = "src/*"          # import { x } from "@app/util"  →  src/util.vlt
//! "@config" = "src/config"    # exact alias
//! ```
//!
//! A pattern has at most one `*`, at its end, and so has its target (both or neither). Targets are
//! relative to the package root and must stay inside it. The longest matching prefix wins, like
//! TypeScript's `compilerOptions.paths`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Check every alias of a `[paths]` table.
pub fn validate(paths: &BTreeMap<String, String>) -> Result<(), String> {
    for (pattern, target) in paths {
        check_alias(pattern, target).map_err(|why| format!("[paths] alias `{pattern}`: {why}"))?;
    }
    Ok(())
}

/// Check one alias; the error says what is wrong with it, without naming it.
pub fn check_alias(pattern: &str, target: &str) -> Result<(), &'static str> {
    if pattern.is_empty() || pattern == "*" {
        return Err("the pattern needs a prefix such as `@app/*`");
    }
    if pattern.starts_with("./") || pattern.starts_with("../") || pattern.starts_with("velt:") {
        return Err("relative and `velt:` specifiers cannot be aliased");
    }
    if !wildcard_ok(pattern) || !wildcard_ok(target) {
        return Err("`*` may only appear once, at the end");
    }
    if pattern.ends_with('*') != target.ends_with('*') {
        return Err("the pattern and its target must both end in `*`, or neither");
    }
    let dir = target.trim_end_matches('*');
    let escapes = Path::new(dir)
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
    if escapes || target.contains('\\') {
        return Err("the target must be a `/`-separated path inside the package");
    }
    Ok(())
}

fn wildcard_ok(s: &str) -> bool {
    s.matches('*').count() == s.ends_with('*') as usize
}

/// The module path (relative to the package root, no extension) that `spec` maps to, using the
/// longest matching alias.
pub fn resolve(paths: &BTreeMap<String, String>, spec: &str) -> Option<PathBuf> {
    let (pattern, target) = paths
        .iter()
        .filter(|(pattern, _)| matches(pattern, spec))
        .max_by_key(|(pattern, _)| pattern.len())?;
    let module = match pattern.strip_suffix('*') {
        Some(prefix) => {
            let rest = &spec[prefix.len()..];
            if rest.split('/').any(|seg| matches!(seg, "" | "." | "..")) {
                return None;
            }
            target.replacen('*', rest, 1)
        }
        None => target.clone(),
    };
    Some(PathBuf::from(module))
}

fn matches(pattern: &str, spec: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => spec.starts_with(prefix) && spec.len() > prefix.len(),
        None => pattern == spec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn longest_prefix_wins() {
        let paths = table(&[
            ("@app/*", "src/*"),
            ("@app/ui/*", "ui/*"),
            ("@config", "src/config"),
        ]);
        assert_eq!(
            resolve(&paths, "@app/util"),
            Some(PathBuf::from("src/util"))
        );
        assert_eq!(
            resolve(&paths, "@app/ui/button"),
            Some(PathBuf::from("ui/button"))
        );
        assert_eq!(
            resolve(&paths, "@config"),
            Some(PathBuf::from("src/config"))
        );
        assert_eq!(resolve(&paths, "@config/x"), None);
        assert_eq!(resolve(&paths, "@app/"), None);
        assert_eq!(resolve(&paths, "@app/../secret"), None);
        assert_eq!(resolve(&paths, "json"), None);
    }

    #[test]
    fn rejects_bad_aliases() {
        for (pattern, target) in [
            ("*", "src/*"),
            ("./x/*", "src/*"),
            ("velt:*", "src/*"),
            ("@a/*/b", "src/*"),
            ("@a/*", "src"),
            ("@a/*", "../outside/*"),
            ("@a", "/abs"),
        ] {
            assert!(
                validate(&table(&[(pattern, target)])).is_err(),
                "`{pattern}` = `{target}` should be rejected"
            );
        }
        assert!(validate(&table(&[("@app/*", "src/*"), ("@cfg", "./src/cfg")])).is_ok());
    }
}
