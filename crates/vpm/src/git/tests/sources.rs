//! One source per package name (#2367, crate::resolve): two transitive git sources for one
//! package are an error until the root names one, and the root's entry then wins, recorded in
//! the lock and told once.

use super::*;

/// Write a package `name` at `subdir` of the upstream repository with `deps`.
fn write(e: &Env, subdir: &str, name: &str, deps: &str) {
    let pkg = e.upstream.dir.join(subdir);
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.vlt"),
        format!("export const pkg: Package = {{ name: \"{name}\", version: \"0.1.0\", dependencies: {{ {deps} }} }};"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.vlt"), format!("// {name}\n")).unwrap();
}

#[test]
fn two_transitive_sources_are_an_error_until_the_root_names_one() {
    let e = env();
    // `a` and `b` each bring their repository's `util` (a path sibling), at two commits.
    write(&e, "util", "util", "");
    write(&e, "a", "a", "util: { path: \"../util\" }");
    write(&e, "b", "b", "util: { path: \"../util\" }");
    e.upstream.git(&["add", "-A"]);
    e.upstream.git(&["commit", "-q", "-m", "one"]);
    let one = e.upstream.git(&["rev-parse", "HEAD"]);
    std::fs::write(e.upstream.dir.join("util/src/lib.vlt"), "// two\n").unwrap();
    e.upstream.git(&["commit", "-q", "-am", "two"]);
    let two = e.upstream.git(&["rev-parse", "HEAD"]);
    let url = e.upstream.url();
    let deps = format!(
        "a: {{ git: \"{url}\", rev: \"{one}\", path: \"a\" }}, b: {{ git: \"{url}\", rev: \"{two}\", path: \"b\" }}"
    );
    let app = e.app(&deps);
    let err = e.install(&app, false).unwrap_err();
    assert!(
        err.contains("`util` is asked for from two sources (name `util` in this package's dependencies to choose one")
            && err.contains(&format!("(commit {}), path util", &one[..12]))
            && err.contains(&format!("(commit {}), path util", &two[..12])),
        "{err}"
    );
    // The root names the second: it wins over `a`'s, and the lock and a note say so.
    let app = e.app(&format!(
        "{deps}, util: {{ git: \"{url}\", rev: \"{two}\", path: \"util\" }}"
    ));
    let installed = e.install(&app, false).unwrap();
    let lock = Lockfile::read(&app).unwrap().unwrap();
    assert_eq!(lock.get("util").unwrap().overrides, ["a"]);
    assert!(lock.get("a").unwrap().overrides.is_empty());
    let note = installed
        .notes
        .iter()
        .find(|n| n.starts_with("`util` comes from this package's own entry"))
        .unwrap_or_else(|| panic!("{:?}", installed.notes));
    assert!(
        note.contains(&format!(
            "`a` asks for `util` from {url} (commit {}",
            &one[..12]
        )),
        "{note}"
    );
    assert!(!note.contains("`b` asks"), "{note}");
    let util = &installed.graph.packages()[0].dependencies["util"];
    assert_eq!(
        std::fs::read_to_string(util.join("src/lib.vlt")).unwrap(),
        "// two\n"
    );
}
