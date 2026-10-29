//! Git dependencies on npm packages: a target with a `package.json` and no `package.vlt`.

use super::*;

/// A pnpm workspace upstream: `@demo/shared` and `@demo/signals` (1.2.3), which imports it.
fn npm_monorepo(e: &Env) {
    let dir = &e.upstream.dir;
    let write = |rel: &str, text: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n");
    write(
        "package.json",
        "{ \"name\": \"demo-root\", \"private\": true }\n",
    );
    write(
        "packages/shared/package.json",
        "{ \"name\": \"@demo/shared\", \"version\": \"1.0.0\" }\n",
    );
    write("packages/shared/src/index.ts", "export const x = 1;\n");
    write(
        "packages/signals/package.json",
        "{ \"name\": \"@demo/signals\", \"version\": \"1.2.3\" }\n",
    );
    write(
        "packages/signals/src/index.ts",
        "import { x } from \"@demo/shared\";\nexport const y = x;\n",
    );
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "npm"]);
}

/// The npm packages the root package of `installed` gets from its git dependencies, by name, as
/// paths relative to the upstream repository's checkout.
fn provided(installed: &crate::Installed) -> Vec<String> {
    installed.graph.packages()[0]
        .npm
        .iter()
        .map(|(name, dir)| {
            let dir = dir.to_string_lossy().replace('\\', "/");
            let rel = dir.rsplit_once("/packages/").map_or("?", |(_, r)| r);
            format!("{name} packages/{rel}")
        })
        .collect()
}

#[test]
fn one_npm_package_of_a_repository() {
    let e = env();
    npm_monorepo(&e);
    let url = e.upstream.url();
    let app = e.app(&format!(
        "\"@demo/signals\": {{ git: \"{url}\", path: \"packages/signals\" }}"
    ));
    let installed = e.install(&app, false).unwrap();
    assert_eq!(provided(&installed), ["@demo/signals packages/signals"]);
    // No package of the graph: its dependents import it as an npm package.
    assert_eq!(installed.graph.packages().len(), 1);
    let lock = Lockfile::read(&app).unwrap().unwrap();
    let locked = lock.get("@demo/signals").unwrap();
    assert_eq!(locked.version, "1.2.3");
    assert!(
        locked.source.contains("?path=packages/signals#"),
        "{}",
        locked.source
    );
    assert!(locked.checksum.is_some());
}

#[test]
fn a_whole_workspace_of_npm_packages() {
    let e = env();
    npm_monorepo(&e);
    let url = e.upstream.url();
    let app = e.app(&format!("\"@demo/signals\": {{ git: \"{url}\" }}"));
    let installed = e.install(&app, false).unwrap();
    // The root's own package too, as `npm install git+…` installs it.
    let names: Vec<&String> = installed.graph.packages()[0].npm.keys().collect();
    assert_eq!(names, ["@demo/shared", "@demo/signals", "demo-root"]);
    // The dependency's name must be one of them, and its version requirement holds.
    let app = e.app(&format!("\"@demo/nope\": {{ git: \"{url}\" }}"));
    let _ = std::fs::remove_file(app.join("velt.lock.json"));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains(
            "provides no npm package named `@demo/nope` (it has @demo/shared, @demo/signals, demo-root)"
        ),
        "{err}"
    );
    let app = e.app(&format!(
        "\"@demo/signals\": {{ git: \"{url}\", version: \"2\" }}"
    ));
    let _ = std::fs::remove_file(app.join("velt.lock.json"));
    assert!(e.install(&app, false).is_err());
}

#[test]
fn a_directory_with_no_package_is_an_error() {
    let e = env();
    e.upstream.commit_package("lib", "lib", "x");
    let app = e.app(&format!(
        "docs: {{ git: \"{}\", path: \"lib/src\" }}",
        e.upstream.url()
    ));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("has neither a package.vlt nor an npm package"),
        "{err}"
    );
}

#[test]
fn two_git_dependencies_providing_one_package_are_an_error() {
    let e = env();
    npm_monorepo(&e);
    let url = e.upstream.url();
    let app = e.app(&format!(
        "\"@demo/shared\": {{ git: \"{url}\" }}, \"@demo/signals\": {{ git: \"{url}\", path: \"packages/signals\" }}"
    ));
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("both provide the npm package `@demo/signals`; depend on one of them"),
        "{err}"
    );
}

/// What `npm::provided::packages_at` makes of a directory, as an error.
fn provided_error(files: &[(&str, &str)]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    crate::npm::provided::packages_at(tmp.path()).unwrap_err()
}

#[test]
fn workspaces_npm_would_refuse_are_errors() {
    let ws = ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n");
    let err = provided_error(&[
        ws,
        ("packages/a/package.json", "{ \"name\": \"same\" }"),
        ("packages/b/package.json", "{ \"name\": \"same\" }"),
    ]);
    assert_eq!(
        err,
        "has two packages named `same`: `packages/a` and `packages/b`"
    );
    let err = provided_error(&[ws, ("packages/a/package.json", "{ \"name\": ")]);
    assert!(
        err.starts_with("has a `package.json` that doesn't parse: `packages/a/package.json`"),
        "{err}"
    );
    let err = provided_error(&[
        ws,
        ("packages/a/package.json", "{ \"version\": \"1.0.0\" }"),
    ]);
    assert_eq!(
        err,
        "has a workspace package without a `name`: `packages/a/package.json`"
    );
    let err = provided_error(&[("package.json", "not json")]);
    assert!(err.contains("`./package.json`"), "{err}");
}
