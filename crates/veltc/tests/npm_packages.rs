//! npm packages compiled from their TypeScript sources, end to end through the `velt` binary:
//! a pnpm workspace laid out like signalxjs/core (scoped packages whose `package.json`
//! `exports` point at `dist/`, sources in `src/`, an `env.d.ts` declaring `__DEV__`), an app
//! with a scoped name whose `package.vlt` has no `paths` and sets `define`.

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn velt(cwd: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .env("VELT_HOME", cwd.join("../../home"))
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_LAUNCHER")
        .output()
        .unwrap()
}

const APP_MANIFEST: &str = r#"import type { Package } from "velt:package";

export const pkg: Package = {
  name: "@demo/app",
  version: "0.1.0",
  entry: "src/main.ts",
  define: { __DEV__: "false", __VERSION__: "\"1.2.3\"" },
};
"#;

fn workspace(root: &Path) {
    write(
        root,
        "package.json",
        r#"{ "name": "demo", "private": true }"#,
    );
    write(
        root,
        "pnpm-workspace.yaml",
        "packages:\n  - packages/*\n  - apps/*\n",
    );
    write(
        root,
        "packages/reactivity/package.json",
        r#"{
  "name": "@demo/reactivity",
  "type": "module",
  "main": "./dist/index.js",
  "types": "./dist/index.d.ts",
  "exports": {
    ".": { "types": "./dist/index.d.ts", "development": "./dist/index.js", "production": "./dist/index.prod.js", "import": "./dist/index.js" },
    "./internals": { "types": "./dist/internals.d.ts", "import": "./dist/internals.js" }
  }
}"#,
    );
    write(
        root,
        "packages/reactivity/src/env.d.ts",
        "/** Replaced at build time. */\ndeclare const __DEV__: boolean;\ndeclare const __VERSION__: string;\n",
    );
    write(
        root,
        "packages/reactivity/src/index.ts",
        "import { warn } from \"./internals.js\";\n\nexport function describe(): string {\n  if (__DEV__) {\n    warn(\"dev build\");\n    return \"dev\";\n  }\n  return \"prod \" + __VERSION__;\n}\n",
    );
    write(
        root,
        "packages/reactivity/src/internals.ts",
        "export function warn(message: string): void {\n  console.log(\"warn: \" + message);\n}\n\nexport const LIMIT: number = 3;\n",
    );
    // A package whose server build is chosen by condition: `browser` names a DOM-only file.
    write(
        root,
        "packages/platform/package.json",
        r#"{ "name": "@demo/platform", "exports": { ".": { "browser": "./src/dom.ts", "node": "./src/server.ts", "default": "./src/dom.ts" } } }"#,
    );
    write(
        root,
        "packages/platform/src/server.ts",
        "export function where(): string {\n  return \"server\";\n}\n",
    );
    write(
        root,
        "packages/platform/src/dom.ts",
        "export function where(): string {\n  return document.title;\n}\n",
    );
    write(root, "apps/web/package.json", r#"{ "name": "@demo/web" }"#);
    write(root, "apps/web/package.vlt", APP_MANIFEST);
    write(
        root,
        "apps/web/src/main.ts",
        "import { describe } from \"@demo/reactivity\";\nimport { LIMIT } from \"@demo/reactivity/internals\";\nimport { where } from \"@demo/platform\";\n\nconsole.log(describe());\nconsole.log(LIMIT);\nconsole.log(where());\n",
    );
}

#[test]
fn workspace_packages_compile_from_source_with_defines() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let root = tmp.path();
    workspace(root);
    let app = root.join("apps/web");

    let out = velt(&app, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "velt run failed:\n{stderr}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "prod 1.2.3\n3\nserver\n"
    );

    // With `__DEV__` true the dev branch runs.
    let dev = APP_MANIFEST.replace("__DEV__: \"false\"", "__DEV__: \"true\"");
    write(root, "apps/web/package.vlt", &dev);
    let out = velt(&app, &["run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "warn: dev build\ndev\n3\nserver\n"
    );

    // Without a define, the declaration says what to add.
    let none = APP_MANIFEST.replace("__DEV__: \"false\", ", "");
    write(root, "apps/web/package.vlt", &none);
    let out = velt(&app, &["check"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`__DEV__` is declared here but has no value")
            && stderr.contains("`define: { __DEV__: \"false\" }` in package.vlt"),
        "{stderr}"
    );

    // A subpath the package does not export.
    write(root, "apps/web/package.vlt", APP_MANIFEST);
    write(
        root,
        "apps/web/src/main.ts",
        "import { x } from \"@demo/reactivity/src/index\";\n",
    );
    let out = velt(&app, &["check"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "package `@demo/reactivity` does not export `./src/index` (`exports` in its package.json)"
        ),
        "{stderr}"
    );
}
