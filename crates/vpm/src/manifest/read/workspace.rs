//! Decoding `velt.workspace.vlt` ([`crate::workspace`]) with the manifest's reader: the same data
//! subset and bounds, and each check located at the value it rejects.

use std::collections::BTreeMap;

use velt_common::{Diagnostics, FileId};

use super::{read_data, Reader, Shape, Value, ValueKind};
use crate::manifest::{check_registry, check_toolchain, schema, Dependency};
use crate::workspace::{WorkspaceFile, WORKSPACE_FILE};

const WORKSPACE_SHAPE: Shape = Shape {
    file: WORKSPACE_FILE,
    binding: "workspace",
    type_name: "Workspace",
    message: "the workspace file must be one `export const workspace: Workspace = { … }`",
};

/// Read and validate the text of a `velt.workspace.vlt`. Every problem is a diagnostic in `file`.
pub fn read_workspace(file: FileId, src: &str) -> Result<WorkspaceFile, Diagnostics> {
    read_data(file, src, &WORKSPACE_SHAPE, |r, v| r.workspace(v)).map(|(ws, _)| ws)
}

impl Reader<'_> {
    fn workspace(&mut self, value: &Value) -> WorkspaceFile {
        let mut ws = WorkspaceFile {
            members: vec![],
            toolchain: None,
            registry: None,
            dependencies: BTreeMap::new(),
        };
        let Some(fields) = self.object(value, "the workspace") else {
            return ws;
        };
        let mut members = false;
        for (key, v) in fields {
            match key.name.as_str() {
                "members" => {
                    members = true;
                    ws.members = self.members(v);
                }
                "velt" => {
                    if let Some(s) = self.string(v, "velt") {
                        self.check(check_toolchain(s).map_err(|e| format!("velt {e}")), v.span);
                        ws.toolchain = Some(s.to_string());
                    }
                }
                "registry" => {
                    if let Some(s) = self.string(v, "registry") {
                        self.check(check_registry(s), v.span);
                        ws.registry = Some(s.to_string());
                    }
                }
                "dependencies" => ws.dependencies = self.workspace_dependencies(v),
                _ => self.unknown_key(key, schema::WORKSPACE),
            }
        }
        if !members {
            self.error("the workspace is missing `members`", value.span);
        }
        ws
    }

    /// `members`: at least one glob, each [`check_member_pattern`], none twice.
    fn members(&mut self, value: &Value) -> Vec<String> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!("`members` must be an array, not {}", value.kind.describe()),
                value.span,
            );
            return vec![];
        };
        if elems.is_empty() {
            self.error("`members` is empty: list the member packages", value.span);
        }
        let mut out: Vec<String> = vec![];
        for elem in elems {
            let Some(pattern) = self.string(elem, "members") else {
                continue;
            };
            if out.iter().any(|p| p == pattern) {
                self.error(format!("`{pattern}` is listed twice"), elem.span);
                continue;
            }
            self.check(check_member_pattern(pattern), elem.span);
            out.push(pattern.to_string());
        }
        out
    }

    /// The workspace's `dependencies`: a package's, where `"workspace"` has nothing to refer to.
    fn workspace_dependencies(&mut self, value: &Value) -> BTreeMap<String, Dependency> {
        let deps = self.dependencies(value);
        if let ValueKind::Object(fields) = &value.kind {
            for (key, v) in fields {
                if deps.get(&key.name).is_some_and(Dependency::is_workspace) {
                    self.error(
                        format!(
                            "`{}: \"workspace\"` is for members; here, write the requirement itself",
                            key.name
                        ),
                        v.span,
                    );
                }
            }
        }
        deps
    }
}

/// A `members` glob: `/`-separated, relative, inside the workspace; `*` and `**` as in
/// [`crate::npm::workspace::package_dirs`], an optional `!` in front to exclude.
fn check_member_pattern(pattern: &str) -> Result<(), String> {
    let body = pattern.strip_prefix('!').unwrap_or(pattern);
    let bad = |why: &str| Err(format!("member pattern `{pattern}`: {why}"));
    if body.is_empty() {
        return bad("empty");
    }
    if body.contains('\\') {
        return bad("separate directories with `/`");
    }
    if body.starts_with('/') || body.as_bytes().get(1) == Some(&b':') {
        return bad("must be relative to the workspace root");
    }
    if body == "." {
        return match pattern.starts_with('!') {
            true => bad("the root can't be excluded; leave out `\".\"`"),
            false => Ok(()),
        };
    }
    for seg in body.split('/') {
        match seg {
            "" | "." => return bad("write it without `./`, `//` or a trailing `/`"),
            ".." => return bad("members must be inside the workspace (no `..`)"),
            s if s.contains("**") && s != "**" => return bad("`**` must be a whole segment"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(body: &str) -> Result<WorkspaceFile, Vec<String>> {
        let src =
            format!("import type {{ Workspace }} from \"velt:package\";\n\nexport const workspace: Workspace = {body};\n");
        read_workspace(FileId(0), &src).map_err(|d| d.into_iter().map(|d| d.message).collect())
    }

    #[test]
    fn reads_every_field() {
        let ws = read(
            r#"{ members: ["packages/*", "!packages/old", "."], velt: "0.2",
                 registry: "https://r.example.com", dependencies: { json: "1.2", util: { path: "vendor/util" } } }"#,
        )
        .unwrap();
        assert_eq!(ws.members, ["packages/*", "!packages/old", "."]);
        assert_eq!(ws.toolchain.as_deref(), Some("0.2"));
        assert_eq!(ws.registry.as_deref(), Some("https://r.example.com"));
        assert_eq!(ws.dependencies["json"], Dependency::Version("1.2".into()));
        assert_eq!(ws.dependencies["util"].path(), Some("vendor/util"));
    }

    #[test]
    fn members_are_required_and_checked() {
        assert_eq!(
            read("{}").unwrap_err(),
            ["the workspace is missing `members`"]
        );
        assert_eq!(
            read("{ members: [] }").unwrap_err(),
            ["`members` is empty: list the member packages"]
        );
        for (pattern, why) in [
            ("../x", "members must be inside the workspace (no `..`)"),
            ("/abs", "must be relative to the workspace root"),
            ("./a", "write it without `./`, `//` or a trailing `/`"),
            ("a/", "write it without `./`, `//` or a trailing `/`"),
            ("a\\\\b", "separate directories with `/`"),
            ("a**", "`**` must be a whole segment"),
            ("!.", "the root can't be excluded; leave out `\".\"`"),
        ] {
            let errs = read(&format!("{{ members: [\"{pattern}\"] }}")).unwrap_err();
            assert_eq!(errs.len(), 1, "{pattern}: {errs:?}");
            assert!(errs[0].ends_with(why), "{pattern}: {}", errs[0]);
        }
        assert_eq!(
            read(r#"{ members: ["a/*", "a/*"] }"#).unwrap_err(),
            ["`a/*` is listed twice"]
        );
    }

    #[test]
    fn rejects_other_shapes_and_keys() {
        let errs = read(r#"{ members: ["a"], memebers: ["b"] }"#).unwrap_err();
        assert_eq!(errs, ["unknown key `memebers` in the manifest"]);
        let pkg = "import type { Package } from \"velt:package\";\nexport const pkg: Package = { name: \"a\", version: \"0.1.0\" };\n";
        let errs: Vec<String> = read_workspace(FileId(0), pkg)
            .unwrap_err()
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert_eq!(errs, [WORKSPACE_SHAPE.message]);
        let errs = read(r#"{ members: ["a"], dependencies: { json: "workspace" } }"#).unwrap_err();
        assert_eq!(
            errs,
            ["`json: \"workspace\"` is for members; here, write the requirement itself"]
        );
    }
}
