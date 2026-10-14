use super::*;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace laid out like signalxjs/core: scoped packages whose `exports` point at `dist/`,
/// sources in `src/`.
fn sigx_like(root: &Path) {
    write(root, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    write(
        root,
        "package.json",
        r#"{ "name": "root", "private": true }"#,
    );
    write(
        root,
        "packages/reactivity/package.json",
        r#"{
            "name": "@sigx/reactivity",
            "main": "./dist/index.js",
            "types": "./dist/index.d.ts",
            "exports": {
                ".": { "types": "./dist/index.d.ts", "development": "./dist/index.js", "import": "./dist/index.js" },
                "./internals": { "types": "./dist/internals.d.ts", "import": "./dist/internals.js" }
            }
        }"#,
    );
    write(root, "packages/reactivity/src/index.ts", "");
    write(root, "packages/reactivity/src/internals.ts", "");
    write(
        root,
        "packages/sigx/package.json",
        r#"{ "name": "sigx", "exports": { ".": { "import": "./dist/sigx.js", "types": "./dist/index.d.ts" } } }"#,
    );
    write(root, "packages/sigx/src/index.ts", "");
    write(
        root,
        "packages/server/package.json",
        r#"{ "name": "@sigx/server", "exports": { "./server": { "import": "./build/out/server/index.js" } } }"#,
    );
    write(
        root,
        "packages/server/tsconfig.json",
        "{\n  // JSONC\n  \"compilerOptions\": { \"outDir\": \"./build/out\", \"rootDir\": \"./lib\", },\n}",
    );
    write(root, "packages/server/lib/server/index.ts", "");
    write(root, "packages/server/src/main.ts", "");
}

#[test]
fn workspace_packages_resolve_to_their_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sigx_like(root);
    let importer = root.join("packages/server/src/main.ts");
    let mut npm = NpmResolver::default();
    let resolve = |npm: &mut NpmResolver, spec: &str| npm.resolve(&importer, spec);
    assert_eq!(
        resolve(&mut npm, "@sigx/reactivity").unwrap().unwrap(),
        root.join("packages/reactivity/src/index.ts")
    );
    assert_eq!(
        resolve(&mut npm, "@sigx/reactivity/internals")
            .unwrap()
            .unwrap(),
        root.join("packages/reactivity/src/internals.ts")
    );
    // `import` comes first and names `dist/sigx.js` (no `src/sigx.ts`); `types` maps.
    assert_eq!(
        resolve(&mut npm, "sigx").unwrap().unwrap(),
        root.join("packages/sigx/src/index.ts")
    );
    // tsconfig's outDir/rootDir, and a package importing itself.
    assert_eq!(
        resolve(&mut npm, "@sigx/server/server").unwrap().unwrap(),
        root.join("packages/server/lib/server/index.ts")
    );
    // Not a package of the workspace: the caller's error.
    assert!(resolve(&mut npm, "left-pad").is_none());
    assert!(resolve(&mut npm, "node:fs").is_none());
    let (msg, _) = resolve(&mut npm, "@sigx/reactivity/missing")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        msg,
        "package `@sigx/reactivity` does not export `./missing` (`exports` in its package.json)"
    );
}

#[test]
fn node_modules_wins_and_main_without_exports() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    sigx_like(root);
    write(
        root,
        "packages/server/node_modules/@sigx/reactivity/package.json",
        r#"{ "name": "@sigx/reactivity", "main": "lib/index.js" }"#,
    );
    write(
        root,
        "packages/server/node_modules/@sigx/reactivity/src/index.ts",
        "",
    );
    write(
        root,
        "node_modules/plain/package.json",
        r#"{ "name": "plain", "main": "./out/main.js" }"#,
    );
    write(root, "node_modules/plain/src/main.ts", "");
    write(root, "node_modules/plain/src/util/index.tsx", "");
    write(
        root,
        "node_modules/js-only/package.json",
        r#"{ "main": "index.js" }"#,
    );
    write(root, "node_modules/js-only/index.js", "");
    let importer = root.join("packages/server/src/main.ts");
    let mut npm = NpmResolver::default();
    assert_eq!(
        npm.resolve(&importer, "@sigx/reactivity").unwrap().unwrap(),
        root.join("packages/server/node_modules/@sigx/reactivity/src/index.ts")
    );
    assert_eq!(
        npm.resolve(&importer, "plain").unwrap().unwrap(),
        root.join("node_modules/plain/src/main.ts")
    );
    assert_eq!(
        npm.resolve(&importer, "plain/src/util").unwrap().unwrap(),
        root.join("node_modules/plain/src/util/index.tsx")
    );
    let (msg, notes) = npm.resolve(&importer, "js-only").unwrap().unwrap_err();
    assert_eq!(
        msg,
        "package `js-only` maps `js-only` to `index.js`, which has no TypeScript source in the package"
    );
    assert!(notes[0].contains("`index.ts`"), "{notes:?}");
}

#[test]
fn splits_specifiers() {
    assert_eq!(
        split_specifier("@sigx/reactivity/internals"),
        Some(("@sigx/reactivity", "./internals".to_string()))
    );
    assert_eq!(split_specifier("sigx"), Some(("sigx", ".".to_string())));
    assert_eq!(split_specifier("a/b/c"), Some(("a", "./b/c".to_string())));
    for bad in [
        "@sigx", "@sigx/", "./x", "node:fs", "a/../b", "a//b", "_x", ".x", "a\\b",
    ] {
        assert_eq!(split_specifier(bad), None, "{bad}");
    }
}

#[test]
fn maps_targets_back_to_sources() {
    let out = vec![("build".to_string(), "lib".to_string())];
    assert_eq!(
        source_candidates("./dist/server/index.d.ts", &[])[..2],
        ["src/server/index.ts", "src/server/index.tsx"]
    );
    assert_eq!(source_candidates("./src/a.ts", &[]), ["src/a.ts"]);
    assert_eq!(source_candidates("./build/a.mjs", &out)[0], "lib/a.ts");
}

/// Link directory `link` to `target` (a symlink, or a junction on Windows when symlinks need
/// privileges); `false` when the OS allows neither.
fn link_dir(target: &Path, link: &Path) -> bool {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        let back = |p: &Path| p.to_string_lossy().replace('/', "\\");
        std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(back(link))
                .arg(back(target))
                .output()
                .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

#[test]
fn pnpm_layout_finds_dependencies_of_dependencies() {
    // node_modules/a → .pnpm/a@1.0.0/node_modules/a; `b` is only next to `a` in `.pnpm`, where
    // Node finds it because it loads `a` from its real path.
    let tmp = tempfile::tempdir().unwrap();
    let root = &crate::relpath::plain(&std::fs::canonicalize(tmp.path()).unwrap());
    let store = root.join("node_modules/.pnpm/a@1.0.0/node_modules");
    write(
        &store,
        "a/package.json",
        r#"{ "name": "a", "exports": { ".": { "types": "./dist/index.d.ts", "import": "./dist/index.js" } } }"#,
    );
    write(
        &store,
        "a/src/index.ts",
        "export * from \"b\";
",
    );
    write(
        &store,
        "b/package.json",
        r#"{ "name": "b", "exports": "./src/index.ts" }"#,
    );
    write(&store, "b/src/index.ts", "");
    write(root, "app/main.ts", "");
    if !link_dir(&store.join("a"), &root.join("node_modules/a")) {
        return;
    }
    let mut npm = NpmResolver::default();
    let a = npm
        .resolve(&root.join("app/main.ts"), "a")
        .unwrap()
        .unwrap();
    assert_eq!(a, store.join("a/src/index.ts"));
    assert_eq!(
        npm.resolve(&a, "b").unwrap().unwrap(),
        store.join("b/src/index.ts")
    );
    // Not a dependency of the app itself.
    assert!(npm.resolve(&root.join("app/main.ts"), "b").is_none());
}

#[test]
fn workspace_symlink_loops_end_fast() {
    // #1091 review: `packages: ["**"]` with links back up used to run for minutes.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "pnpm-workspace.yaml",
        "packages:
  - \"**\"
",
    );
    write(root, "packages/p/package.json", r#"{ "name": "p" }"#);
    write(root, "packages/p/src/main.ts", "");
    if !link_dir(&root.join("packages"), &root.join("packages/loop1"))
        || !link_dir(&root.join("packages"), &root.join("packages/p/loop2"))
    {
        return;
    }
    let started = std::time::Instant::now();
    let mut npm = NpmResolver::default();
    assert!(npm
        .resolve(&root.join("packages/p/src/main.ts"), "missing")
        .is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let dirs = workspace::package_dirs(root, &["**".to_string()]);
    let rel: Vec<String> = dirs
        .dirs
        .iter()
        .map(|d| crate::relpath::relative(d, root))
        .collect();
    assert_eq!(rel, ["packages/p"]);
}

#[test]
fn a_cut_workspace_walk_is_reported() {
    // #1091 re-check: `packages: ["**"]` over a huge tree stopped at the limit and the
    // package past it was then "not a dependency", without a word about the cut.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "pnpm-workspace.yaml", "packages: [\"**\"]\n");
    for i in 0..5 {
        std::fs::create_dir_all(root.join(format!("a/{i}"))).unwrap();
    }
    write(root, "zzz/target/package.json", r#"{ "name": "target" }"#);
    write(root, "zzz/target/src/index.ts", "");
    let importer = root.join("app/main.ts");
    let mut npm = NpmResolver {
        max_dirs: Some(4),
        ..NpmResolver::default()
    };
    assert!(npm.resolve(&importer, "target").is_none());
    let note = npm.workspace_note(&importer).unwrap();
    assert!(
        note.contains("pattern `**` reaches more than 4 directories"),
        "{note}"
    );
    assert!(note.contains("narrow it"), "{note}");
    // Under the limit the package is found and there is nothing to say.
    let mut npm = NpmResolver::default();
    assert!(npm.resolve(&importer, "target").unwrap().is_ok());
    assert_eq!(npm.workspace_note(&importer), None);
}

#[test]
fn a_later_condition_is_no_fallback() {
    // #1091 review, #929 gap 2: Node loads `server.js`; its source is missing, so the client
    // build must not be compiled in its place.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "node_modules/iso/package.json",
        r#"{ "name": "iso", "exports": { ".": { "node": "./dist/server.js", "default": "./dist/client.js" } } }"#,
    );
    write(root, "node_modules/iso/src/client.ts", "");
    write(root, "main.ts", "");
    let mut npm = NpmResolver::default();
    let (msg, notes) = npm
        .resolve(&root.join("main.ts"), "iso")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        msg,
        "package `iso` maps `iso` to `dist/server.js`, which has no TypeScript source in the package"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("Node loads `dist/server.js` (condition `node`)")),
        "{notes:?}"
    );
    write(root, "node_modules/iso/src/server.ts", "");
    assert_eq!(
        npm.resolve(&root.join("main.ts"), "iso").unwrap().unwrap(),
        root.join("node_modules/iso/src/server.ts")
    );
}
