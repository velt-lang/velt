//! The `velt` field of the nearest `package.vlt`, or of the `velt.workspace.vlt` of its workspace,
//! read on its own: the launcher reads it before every command, so it does not decode (or check)
//! the rest of the file. vpm's reader checks the whole file, this field included, when the
//! selected toolchain runs.

use std::path::{Path, PathBuf};

use velt_common::FileId;
use velt_syntax::ast::{ExprKind, ItemKind, Lit, ObjectProp, PatternKind};

use crate::requirement::Requirement;

/// The manifest's file name (`vpm::manifest::MANIFEST_FILE`).
pub const MANIFEST_FILE: &str = "package.vlt";
/// The workspace file's name (`vpm::workspace::WORKSPACE_FILE`).
pub const WORKSPACE_FILE: &str = "velt.workspace.vlt";
/// The pinning field.
pub const FIELD: &str = "velt";

/// A package's toolchain requirement and where it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub requirement: Requirement,
    /// The `package.vlt` or `velt.workspace.vlt` it is in.
    pub manifest: PathBuf,
    /// 1-based line of the value.
    pub line: usize,
}

/// The package's lockfile, beside its `package.vlt` (`vpm::lockfile::LOCK_FILE`).
pub const LOCK_FILE: &str = "velt.lock.json";

/// The toolchain version the lockfile beside `manifest` records (its `velt` field, #960), if
/// any. A missing or unreadable lock, or one without the field, is `None`: the toolchain that
/// runs reports a broken lock.
pub fn locked_version(manifest: &Path) -> Option<semver::Version> {
    let text = std::fs::read_to_string(manifest.with_file_name(LOCK_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    semver::Version::parse(value.get("velt")?.as_str()?).ok()
}

/// The nearest directory at or above `start` holding a `package.vlt`.
pub fn find_package_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(MANIFEST_FILE).is_file())
        .map(Path::to_path_buf)
}

/// The pin of the package enclosing `dir` (an absolute path): its own `velt` field, or its
/// workspace's when the workspace's `members` list it (members leave it to `velt.workspace.vlt`),
/// or the workspace's when `dir` is in a workspace but no package of it. A package below a
/// workspace that the globs don't list keeps to itself. `None` outside both, without a `velt` field, or when the file
/// does not parse (the toolchain then reports why); an error when the field is there but is not
/// a requirement.
pub fn find_pin(dir: &Path) -> Result<Option<Pin>, String> {
    let package = find_package_root(dir);
    let workspace = dir
        .ancestors()
        .find(|d| d.join(WORKSPACE_FILE).is_file())
        .map(Path::to_path_buf);
    let package_nearer = match (&package, &workspace) {
        (Some(pkg), Some(ws)) => pkg.starts_with(ws),
        (pkg, _) => pkg.is_some(),
    };
    if let Some(root) = package.as_ref().filter(|_| package_nearer) {
        if let Some(pin) = pin_in(&root.join(MANIFEST_FILE), "pkg")? {
            return Ok(Some(pin));
        }
    }
    let Some(ws) = workspace else {
        return Ok(None);
    };
    let file = ws.join(WORKSPACE_FILE);
    if let Some(pkg) = package.filter(|p| p.starts_with(&ws)) {
        let src = std::fs::read_to_string(&file).unwrap_or_default();
        let rel: Vec<String> = pkg
            .strip_prefix(&ws)
            .unwrap_or(Path::new(""))
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let rel = if rel.is_empty() {
            ".".to_string()
        } else {
            rel.join("/")
        };
        // A file that does not parse is the toolchain's to report: its pin still counts.
        if read_members(&src).is_some_and(|globs| !crate::members::lists(&globs, &rel)) {
            return Ok(None);
        }
    }
    pin_in(&file, "workspace")
}

/// The `members` strings of the workspace file text `src`, when it parses and has them.
fn read_members(src: &str) -> Option<Vec<String>> {
    let (module, diags) = velt_syntax::parse_file(FileId(0), src);
    if diags.iter().any(velt_common::Diagnostic::is_error) {
        return None;
    }
    let props = module.items.iter().find_map(|item| match &item.kind {
        ItemKind::Var(var)
            if item.exported
                && matches!(&var.pattern.kind, PatternKind::Ident(id) if id.name == "workspace") =>
        {
            match var.init.as_ref().map(|e| &e.kind) {
                Some(ExprKind::Object(props)) => Some(props),
                _ => None,
            }
        }
        _ => None,
    })?;
    let members = props.iter().find_map(|p| match p {
        ObjectProp::KeyValue(key, value) if key.name == "members" => Some(value),
        _ => None,
    })?;
    let ExprKind::Array(elems) = &members.kind else {
        return None;
    };
    elems
        .iter()
        .map(|e| match &e.kind {
            ExprKind::Lit(Lit::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// The `velt` field of the `binding` object of the file `path`.
fn pin_in(path: &Path, binding: &str) -> Result<Option<Pin>, String> {
    let manifest = path.to_path_buf();
    let src = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    read_pin_of(&src, binding)
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
    read_pin_of(src, "pkg")
}

/// The `velt` field of the object `export const <binding>` in `src` (`pkg` in a `package.vlt`,
/// `workspace` in a `velt.workspace.vlt`).
fn read_pin_of(src: &str, binding: &str) -> Result<Option<(Requirement, usize)>, (String, usize)> {
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
            || !matches!(&var.pattern.kind, PatternKind::Ident(id) if id.name == binding)
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

    const HEADER: &str = "import type { Package } from \"velt/package\";\n\n";

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

    #[test]
    fn a_member_s_pin_is_its_workspace_s() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        let member = ws.join("packages/core");
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(
            member.join(MANIFEST_FILE),
            format!("{HEADER}export const pkg: Package = {{ name: \"core\" }};\n"),
        )
        .unwrap();
        std::fs::write(
            ws.join(WORKSPACE_FILE),
            "export const workspace: Workspace = { members: [\"packages/*\"], velt: \"0.3\" };\n",
        )
        .unwrap();
        for dir in [member.join("src"), ws.join("packages"), ws.clone()] {
            let found = find_pin(&dir).unwrap().unwrap();
            assert_eq!(found.requirement.as_str(), "0.3", "{}", dir.display());
            assert_eq!(found.manifest, ws.join(WORKSPACE_FILE));
        }
        // A package below the workspace that no glob lists keeps to itself.
        let other = ws.join("tools/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join(MANIFEST_FILE),
            format!("{HEADER}export const pkg: Package = {{ name: \"other\" }};\n"),
        )
        .unwrap();
        assert_eq!(find_pin(&other).unwrap(), None);
        // The launcher reads the lock beside the file holding the pin: the workspace's.
        std::fs::write(ws.join(LOCK_FILE), r#"{"version": 1, "velt": "0.3.1"}"#).unwrap();
        let found = find_pin(&member).unwrap().unwrap();
        assert_eq!(
            locked_version(&found.manifest).unwrap().to_string(),
            "0.3.1"
        );
    }

    #[test]
    fn reads_the_toolchain_the_lock_records() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join(MANIFEST_FILE);
        assert_eq!(locked_version(&manifest), None);
        let lock = tmp.path().join(LOCK_FILE);
        std::fs::write(&lock, r#"{"version": 1, "packages": []}"#).unwrap();
        assert_eq!(locked_version(&manifest), None);
        std::fs::write(&lock, r#"{"version": 1, "velt": "0.2.1", "packages": []}"#).unwrap();
        assert_eq!(locked_version(&manifest).unwrap().to_string(), "0.2.1");
        std::fs::write(&lock, r#"{"version": 1, "velt": "latest"}"#).unwrap();
        assert_eq!(locked_version(&manifest), None);
        std::fs::write(&lock, "not json").unwrap();
        assert_eq!(locked_version(&manifest), None);
    }
}
