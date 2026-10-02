//! `velt add`'s edit of a user's `package.vlt`: the new dependency is spliced into the source text
//! at syntax-tree spans and the file is formatted with `velt fmt`, so comments survive.

use std::path::Path;

use velt_common::{FileId, Span};
use velt_syntax::ast::{Expr, ExprKind, ItemKind, ObjectProp};

use crate::manifest::write::{dependency_lit, key};
use crate::manifest::{
    check_dependency, is_valid_package_name, Dependency, DetailedDependency, Manifest,
    MANIFEST_FILE,
};

/// What `velt add` writes for one dependency.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencySpec {
    /// Semver requirement.
    pub version: Option<String>,
    /// Local path, relative to the manifest.
    pub path: Option<String>,
}

/// Add or replace `dependencies.<name>` in the manifest text; returns the new text (validated).
pub fn add_dependency_text(
    text: &str,
    name: &str,
    spec: &DependencySpec,
) -> Result<String, String> {
    if !is_valid_package_name(name) {
        return Err(format!("invalid package name `{name}`"));
    }
    let dep = match (&spec.version, &spec.path) {
        (Some(v), None) => Dependency::Version(v.clone()),
        (None, None) => return Err(format!("dependency `{name}` needs a version or a path")),
        (version, Some(path)) => Dependency::Detailed(DetailedDependency {
            version: version.clone(),
            path: Some(path.clone()),
        }),
    };
    // Checked before editing, so the error is not about the edited text.
    check_dependency(name, &dep)?;
    // Only a valid manifest is edited, so its shape below is known.
    Manifest::parse(text)?;
    let (module, _) = velt_syntax::parse_file(FileId(0), text);
    let pkg = module
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Var(var) => var.init.as_ref(),
            _ => None,
        })
        .expect("ICE: a valid manifest has a `pkg` object");
    let value = dependency_lit(&dep);
    let entry = format!("{}: {value}", key(name));
    let edits = match find(pkg, "dependencies") {
        Some(deps) => match find(deps, name) {
            Some(old) => vec![Splice::replace(old.span, value)],
            None => insert(text, deps, entry),
        },
        None => insert(text, pkg, format!("dependencies: {{ {entry} }}")),
    };
    let spliced = apply(text, &edits);
    let out = velt_fmt::format_source(&spliced)
        .map_err(|_| format!("ICE: editing {MANIFEST_FILE} produced invalid Velt:\n{spliced}"))?;
    Manifest::parse(&out)?;
    Ok(out)
}

/// [`add_dependency_text`] applied to `<root>/package.vlt` on disk.
pub fn add_dependency(root: &Path, name: &str, spec: &DependencySpec) -> Result<(), String> {
    let path = root.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let out = add_dependency_text(&text, name, spec)?;
    std::fs::write(&path, out).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Replace the bytes `lo..hi` with `text`.
struct Splice {
    lo: usize,
    hi: usize,
    text: String,
}

impl Splice {
    fn replace(span: Span, text: String) -> Splice {
        Splice {
            lo: span.lo as usize,
            hi: span.hi as usize,
            text,
        }
    }

    fn insert(at: usize, text: String) -> Splice {
        Splice {
            lo: at,
            hi: at,
            text,
        }
    }
}

/// `src` with `edits` (in source order, not overlapping) applied.
fn apply(src: &str, edits: &[Splice]) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    let mut at = 0;
    for e in edits {
        out.push_str(&src[at..e.lo]);
        out.push_str(&e.text);
        at = e.hi;
    }
    out.push_str(&src[at..]);
    out
}

/// The value of property `key` of an object literal.
fn find<'e>(object: &'e Expr, key: &str) -> Option<&'e Expr> {
    let ExprKind::Object(props) = &object.kind else {
        return None;
    };
    props.iter().find_map(|p| match p {
        ObjectProp::KeyValue(k, v) if k.name == key => Some(v),
        _ => None,
    })
}

/// Add the property `prop` (`key: value`) at the end of an object literal in `src`, right before
/// its `}`, so comments inside the object stay where they are. Without a trailing comma, a `,` is
/// added after the last value as well.
fn insert(src: &str, object: &Expr, prop: String) -> Vec<Splice> {
    let last = match &object.kind {
        ExprKind::Object(props) => props.iter().rev().find_map(|p| match p {
            ObjectProp::KeyValue(_, v) => Some(v.span.hi as usize),
            _ => None,
        }),
        _ => None,
    };
    let close = object.span.hi as usize - 1;
    let before_close = Splice::insert(close, format!(" {prop} "));
    match last {
        Some(hi) if !has_comma(src, hi, close) => {
            vec![Splice::insert(hi, ",".into()), before_close]
        }
        _ => vec![before_close],
    }
}

/// Whether `src[lo..hi]` has a `,` outside comments.
fn has_comma(src: &str, lo: usize, hi: usize) -> bool {
    let comments = velt_syntax::comment_ranges(src);
    src[lo..hi].char_indices().any(|(i, c)| {
        let at = (lo + i) as u32;
        c == ',' && !comments.iter().any(|r| r.contains(&at))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "\
import type { Package } from \"velt:package\";

// my app
export const pkg: Package = {
  name: \"app\", // the name
  version: \"0.1.0\",
};
";

    fn spec(version: Option<&str>, path: Option<&str>) -> DependencySpec {
        DependencySpec {
            version: version.map(Into::into),
            path: path.map(Into::into),
        }
    }

    #[test]
    fn adds_dependencies_and_keeps_comments() {
        let out = add_dependency_text(BASE, "json", &spec(Some("1.2"), None)).unwrap();
        assert!(out.contains("// my app"), "{out}");
        assert!(out.contains("name: \"app\", // the name"), "{out}");
        assert!(out.contains("dependencies: { json: \"1.2\" }"), "{out}");
        let out = add_dependency_text(&out, "my-util", &spec(None, Some("../util"))).unwrap();
        assert!(out.contains("\"my-util\": { path: \"../util\" }"), "{out}");
        // Replacing keeps a single entry.
        let out = add_dependency_text(&out, "json", &spec(Some("2"), None)).unwrap();
        assert_eq!(out.matches("json").count(), 1, "{out}");
        let m = Manifest::parse(&out).unwrap();
        assert_eq!(m.dependencies["json"].version(), Some("2"));
        assert_eq!(m.dependencies["my-util"].path(), Some("../util"));
        // The result is formatted: formatting it again changes nothing.
        assert_eq!(velt_fmt::format_source(&out).unwrap(), out);
    }

    #[test]
    fn adds_to_an_empty_or_multiline_dependencies_object() {
        let empty = BASE.replace(
            "version: \"0.1.0\",",
            "version: \"0.1.0\",\n  dependencies: {},",
        );
        let out = add_dependency_text(&empty, "json", &spec(Some("1"), None)).unwrap();
        assert!(out.contains("dependencies: { json: \"1\" }"), "{out}");

        let multi = BASE.replace(
            "version: \"0.1.0\",",
            "version: \"0.1.0\",\n  dependencies: {\n    a: \"1\", // first\n  },",
        );
        let out = add_dependency_text(&multi, "b", &spec(Some("2"), Some("../b"))).unwrap();
        assert!(out.contains("a: \"1\", // first"), "{out}");
        assert!(
            out.contains("b: { version: \"2\", path: \"../b\" }"),
            "{out}"
        );
        assert_eq!(Manifest::parse(&out).unwrap().dependencies.len(), 2);
    }

    #[test]
    fn comments_stay_where_they_were() {
        // A comment inside an empty object.
        let empty = BASE.replace(
            "version: \"0.1.0\",",
            "version: \"0.1.0\",\n  dependencies: { /* none yet */ },",
        );
        let out = add_dependency_text(&empty, "json", &spec(Some("1"), None)).unwrap();
        assert!(out.contains("/* none yet */"), "{out}");
        assert_eq!(Manifest::parse(&out).unwrap().dependencies.len(), 1);
        // A comment after the last property, which has no trailing comma.
        let no_comma = BASE.replace("version: \"0.1.0\",", "version: \"0.1.0\" // why");
        let out = add_dependency_text(&no_comma, "json", &spec(Some("1"), None)).unwrap();
        assert!(out.contains("version: \"0.1.0\", // why"), "{out}");
        assert!(out.contains("dependencies: { json: \"1\" }"), "{out}");
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(add_dependency_text(BASE, "Bad", &spec(Some("1"), None)).is_err());
        let err = add_dependency_text(BASE, "x", &spec(Some("not a req"), None)).unwrap_err();
        assert!(
            err.starts_with("dependency `x`: invalid version requirement"),
            "{err}"
        );
        assert!(add_dependency_text(BASE, "x", &spec(None, None)).is_err());
        let broken = BASE.replace("\"app\"", "app");
        assert!(add_dependency_text(&broken, "x", &spec(Some("1"), None)).is_err());
    }
}
