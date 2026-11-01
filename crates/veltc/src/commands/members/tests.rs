use std::path::Path;

use super::*;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn the_selection_is_taken_out_of_the_arguments() {
    let (sel, rest) = extract(&os(&[
        "--workspace",
        "-p",
        "app*",
        "--release",
        "--package=core",
        "--affected",
        "origin/main",
        "--fail-fast",
    ]))
    .unwrap();
    assert!(sel.workspace && sel.fail_fast);
    assert_eq!(sel.packages, ["app*", "core"]);
    assert_eq!(sel.affected, Some(Some("origin/main".into())));
    assert_eq!(rest, os(&["--release"]));

    // `--affected` before a flag has no base; program arguments after `--` stay as they are.
    let (sel, rest) = extract(&os(&["--affected", "--locked", "--", "-p", "x"])).unwrap();
    assert_eq!(sel.affected, Some(None));
    assert_eq!(rest, os(&["--locked", "--", "-p", "x"]));
    assert_eq!(
        extract(&os(&["--affected=HEAD~1"])).unwrap().0.affected,
        Some(Some("HEAD~1".into()))
    );
    assert!(extract(&os(&["-p"]))
        .unwrap_err()
        .contains("needs a member name"));
    assert!(extract(&os(&["-p", "--release"])).is_err());
}

#[test]
fn files_are_told_from_flag_values() {
    assert_eq!(
        positional(&os(&["--target", "x86_64-unknown-linux-gnu", "-v"])),
        None
    );
    assert_eq!(
        positional(&os(&["--filter", "parses", "src/a.vlt"])).as_deref(),
        Some("src/a.vlt")
    );
    assert_eq!(positional(&os(&["--", "file"])), None);
}

/// A workspace: `core`, `util` (uses core), `app` (uses util) and `tool`.
fn workspace(root: &Path) -> Workspace {
    std::fs::write(
        root.join(vpm::workspace::WORKSPACE_FILE),
        "export const workspace: Workspace = { members: [\"*\"] };",
    )
    .unwrap();
    for (name, deps) in [
        ("app", "util: \"workspace\""),
        ("core", ""),
        ("tool", ""),
        ("util", "core: \"workspace\""),
    ] {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("package.vlt"),
            format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\", dependencies: {{ {deps} }} }};"),
        )
        .unwrap();
    }
    Workspace::load(root).unwrap()
}

fn names(members: &[&Member]) -> Vec<String> {
    members.iter().map(|m| m.name().to_string()).collect()
}

#[test]
fn members_come_in_dependency_order() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = workspace(&vpm::relpath::absolute(tmp.path()));
    let all = Selection {
        workspace: true,
        ..Selection::default()
    };
    assert_eq!(
        names(&select(&ws, &all).unwrap()),
        ["core", "util", "app", "tool"]
    );
    let some = Selection {
        packages: vec!["app".into(), "c*".into()],
        ..Selection::default()
    };
    assert_eq!(names(&select(&ws, &some).unwrap()), ["core", "app"]);
    let none = Selection {
        packages: vec!["nope*".into()],
        ..Selection::default()
    };
    let err = select(&ws, &none).unwrap_err();
    assert!(
        err.contains("`-p nope*` matches no member") && err.contains("app, core, tool, util"),
        "{err}"
    );
}

#[test]
fn dependents_are_found_through_other_members() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = workspace(&vpm::relpath::absolute(tmp.path()));
    let deps = dependents(&ws);
    assert_eq!(deps["core"], BTreeSet::from(["util", "app"]));
    assert_eq!(deps["util"], BTreeSet::from(["app"]));
    assert!(deps["tool"].is_empty());
}

#[test]
fn a_scoped_name_matches_without_its_scope_too() {
    assert!(name_matches("server*", "@org/server"));
    assert!(name_matches("@org/*", "@org/server"));
    assert!(name_matches("*", "@org/server"));
    assert!(!name_matches("org*", "@org/server"));
    assert!(name_matches("core", "core"));
}

#[test]
fn some_options_need_one_member() {
    assert!(one_member_only(&os(&["--watch"]))
        .unwrap()
        .contains("until it is stopped"));
    assert!(one_member_only(&os(&["-o", "x"]))
        .unwrap()
        .contains("same file"));
    assert!(one_member_only(&os(&["--output=x"])).is_some());
    assert!(one_member_only(&os(&["--json"])).is_some());
    assert_eq!(one_member_only(&os(&["--release", "-v"])), None);
}

#[test]
fn a_relative_output_stays_relative_to_where_velt_started() {
    let cwd = Path::new("/work/repo");
    let args = absolute_output(
        os(&["-o", "dist/a", "--release", "--output=x", "--", "-o", "y"]),
        cwd,
    );
    assert_eq!(
        args,
        vec![
            OsString::from("-o"),
            cwd.join("dist/a").into_os_string(),
            OsString::from("--release"),
            OsString::from(format!("--output={}", cwd.join("x").display())),
            OsString::from("--"),
            OsString::from("-o"),
            OsString::from("y"),
        ]
    );
}
