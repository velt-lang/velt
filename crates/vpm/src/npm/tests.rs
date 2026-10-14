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
