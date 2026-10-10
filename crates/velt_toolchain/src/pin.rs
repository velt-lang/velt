//! The `velt` field of the nearest `package.vlt`, read on its own: the launcher reads it before
//! every command, so it does not decode (or check) the rest of the manifest. vpm's reader checks
//! the whole manifest, this field included, when the selected toolchain runs.

use std::path::{Path, PathBuf};

use velt_common::FileId;
use velt_syntax::ast::{ExprKind, ItemKind, Lit, ObjectProp, PatternKind};

use crate::requirement::Requirement;

/// The manifest's file name (`vpm::manifest::MANIFEST_FILE`).
pub const MANIFEST_FILE: &str = "package.vlt";
/// The pinning field.
pub const FIELD: &str = "velt";

/// A package's toolchain requirement and where it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub requirement: Requirement,
    /// The `package.vlt` it is in.
    pub manifest: PathBuf,
    /// 1-based line of the value.
    pub line: usize,
}

/// The nearest directory at or above `start` holding a `package.vlt`.
pub fn find_package_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(MANIFEST_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The pin of the package enclosing `dir` (an absolute path). `None` outside a package, without
/// a `velt` field, or when the manifest does not parse (the toolchain then reports why);
/// an error when the field is there but is not a requirement.
pub fn find_pin(dir: &Path) -> Result<Option<Pin>, String> {
    let Some(root) = find_package_root(dir) else {
        return Ok(None);
    };
    let manifest = root.join(MANIFEST_FILE);
    let src = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    read_pin(&src)
        .map(|found| {
            found.map(|(requirement, line)| Pin {
                requirement,
                manifest: manifest.clone(),
                line,
            })
        })
        .map_err(|(e, line)| format!("{}:{line}: {e}", manifest.display()))
}

/// The `velt` field of the manifest text `src`, with its line; the error carries its line too.
pub fn read_pin(src: &str) -> Result<Option<(Requirement, usize)>, (String, usize)> {
    let (module, diags) = velt_syntax::parse_file(FileId(0), src);
    if diags.iter().any(velt_common::Diagnostic::is_error) {
        return Ok(None);
    }
    let line = |offset: u32| src[..offset as usize].matches('\n').count() + 1;
    for item in &module.items {
        let ItemKind::Var(var) = &item.kind else {
            continue;
        };
        if !item.exported
            || !matches!(&var.pattern.kind, PatternKind::Ident(id) if id.name == "pkg")
        {
            continue;
        }
        let Some(ExprKind::Object(props)) = var.init.as_ref().map(|e| &e.kind) else {
            return Ok(None);
        };
        for prop in props {
            let ObjectProp::KeyValue(key, value) = prop else {
                continue;
            };
            if key.name != FIELD {
                continue;
            }
            let at = line(value.span.lo);
            return match &value.kind {
                ExprKind::Lit(Lit::Str(text)) => Requirement::parse(text)
                    .map(|req| Some((req, at)))
                    .map_err(|e| (format!("`{FIELD}`: {e}"), at)),
                _ => Err((format!("`{FIELD}` must be a string"), at)),
            };
        }
        return Ok(None);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "import type { Package } from \"velt:package\";\n\n";

    fn pin(body: &str) -> Result<Option<(String, usize)>, (String, usize)> {
        read_pin(&format!("{HEADER}export const pkg: Package = {body};\n"))
            .map(|p| p.map(|(req, line)| (req.to_string(), line)))
    }

    #[test]
    fn reads_the_velt_field() {
        assert_eq!(
            pin("{ name: \"a\", version: \"0.1.0\", velt: \"0.1\" }").unwrap(),
            Some(("0.1".into(), 3))
        );
        assert_eq!(
            pin("{\n  name: \"a\",\n  velt: \"=0.1.3\",\n}").unwrap(),
            Some(("=0.1.3".into(), 5))
        );
        assert_eq!(pin("{ name: \"a\", version: \"0.1.0\" }").unwrap(), None);
    }

    #[test]
    fn a_bad_field_is_an_error_and_a_broken_manifest_is_left_to_the_toolchain() {
        let (err, line) = pin("{ velt: \"latest\" }").unwrap_err();
        assert!(
            err.starts_with("`velt`: `latest` is not") && line == 3,
            "{err}"
        );
        assert_eq!(pin("{ velt: 1 }").unwrap_err().0, "`velt` must be a string");
        assert_eq!(read_pin("export const pkg: Package = {").unwrap(), None);
        assert_eq!(read_pin("").unwrap(), None);
    }

    #[test]
    fn finds_the_enclosing_package() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("app");
        std::fs::create_dir_all(pkg.join("src/deep")).unwrap();
        assert_eq!(find_pin(&pkg.join("src/deep")).unwrap(), None);
        std::fs::write(
            pkg.join(MANIFEST_FILE),
            format!("{HEADER}export const pkg: Package = {{ name: \"app\", velt: \"0.2\" }};\n"),
        )
        .unwrap();
        let found = find_pin(&pkg.join("src/deep")).unwrap().unwrap();
        assert_eq!(found.requirement.as_str(), "0.2");
        assert_eq!(found.manifest, pkg.join(MANIFEST_FILE));
        std::fs::write(
            pkg.join(MANIFEST_FILE),
            "export const pkg: Package = { velt: \"latest\" };",
        )
        .unwrap();
        let err = find_pin(&pkg).unwrap_err();
        assert!(err.contains("package.vlt:1: `velt`"), "{err}");
    }
}
