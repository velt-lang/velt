//! Format-preserving edits of a user's `velt.toml` (comments, ordering and whitespace survive).

use std::path::Path;

use toml_edit::{DocumentMut, InlineTable};

use crate::manifest::{is_valid_package_name, Manifest, MANIFEST_FILE};

/// What `velt add` writes for one dependency.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencySpec {
    /// Semver requirement.
    pub version: Option<String>,
    /// Local path, relative to the manifest.
    pub path: Option<String>,
}

/// Add or replace `[dependencies].<name>` in the manifest text; returns the new text (validated).
pub fn add_dependency_text(
    text: &str,
    name: &str,
    spec: &DependencySpec,
) -> Result<String, String> {
    if !is_valid_package_name(name) {
        return Err(format!("invalid package name `{name}`"));
    }
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("invalid {MANIFEST_FILE}: {e}"))?;
    let value = match (&spec.version, &spec.path) {
        (Some(v), None) => toml_edit::value(v.as_str()),
        (None, None) => return Err(format!("dependency `{name}` needs a version or a path")),
        (version, Some(path)) => {
            let mut t = InlineTable::new();
            if let Some(v) = version {
                t.insert("version", v.as_str().into());
            }
            t.insert("path", path.as_str().into());
            toml_edit::value(t)
        }
    };
    if !doc.contains_key("dependencies") {
        doc["dependencies"] = toml_edit::table();
    }
    let deps = doc["dependencies"]
        .as_table_like_mut()
        .ok_or_else(|| format!("`dependencies` in {MANIFEST_FILE} is not a table"))?;
    deps.insert(name, value);
    let out = doc.to_string();
    Manifest::parse(&out)?;
    Ok(out)
}

/// [`add_dependency_text`] applied to `<root>/velt.toml` on disk.
pub fn add_dependency(root: &Path, name: &str, spec: &DependencySpec) -> Result<(), String> {
    let path = root.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let out = add_dependency_text(&text, name, spec)?;
    std::fs::write(&path, out).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "# my app\n[package]\nname = \"app\"   # the name\nversion = \"0.1.0\"\n";

    fn spec(version: Option<&str>, path: Option<&str>) -> DependencySpec {
        DependencySpec {
            version: version.map(Into::into),
            path: path.map(Into::into),
        }
    }

    #[test]
    fn adds_table_and_preserves_formatting() {
        let out = add_dependency_text(BASE, "json", &spec(Some("1.2"), None)).unwrap();
        assert!(out.starts_with(BASE), "{out}");
        assert!(out.contains("[dependencies]\njson = \"1.2\"\n"), "{out}");
        let out = add_dependency_text(&out, "util", &spec(None, Some("../util"))).unwrap();
        assert!(out.contains("util = { path = \"../util\" }"), "{out}");
        // Replacing keeps a single entry.
        let out = add_dependency_text(&out, "json", &spec(Some("2"), None)).unwrap();
        assert_eq!(out.matches("json").count(), 1, "{out}");
        assert_eq!(
            Manifest::parse(&out).unwrap().dependencies["json"].version(),
            Some("2")
        );
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(add_dependency_text(BASE, "Bad", &spec(Some("1"), None)).is_err());
        assert!(add_dependency_text(BASE, "x", &spec(Some("not a req"), None)).is_err());
        assert!(add_dependency_text(BASE, "x", &spec(None, None)).is_err());
    }
}
