use super::*;
use std::collections::BTreeMap;

/// A module with custom sections `(name, content)`, a type section between them.
fn module(sections: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    for (name, content) in sections {
        out.extend([1, 1, 0]);
        let mut body = vec![name.len() as u8];
        body.extend(name.as_bytes());
        body.extend(*content);
        out.push(0);
        // A two-byte size, as LEB128 allows.
        out.extend([(body.len() as u8 & 0x7f) | 0x80, 0]);
        out.extend(body);
    }
    out
}

/// A host package `host-edge` at `root` with its entry and a scaffolded file.
fn host_package(root: &Path) -> HostConfig {
    std::fs::create_dir_all(root.join("src/conf")).unwrap();
    std::fs::write(root.join("src/entry.mjs"), "// the platform's entry\n").unwrap();
    std::fs::write(root.join("src/conf/edge.toml"), "name = \"{{name}}\"\n").unwrap();
    HostConfig {
        target: "wasm32-unknown-unknown".into(),
        condition: "edge".into(),
        entry: "./src/entry.mjs".into(),
        scaffold: BTreeMap::from([("edge.toml".into(), "./src/conf/edge.toml".into())]),
    }
}

fn graph(tmp: &Path) -> PackageGraph {
    let mut graph = PackageGraph::default();
    graph.add("app", &tmp.join("app"), BTreeMap::new());
    let kv = tmp.join("kv");
    std::fs::create_dir_all(kv.join("src/js")).unwrap();
    std::fs::write(kv.join("src/js/kv.mjs"), "export default () => ({});\n").unwrap();
    graph
        .add("kv", &kv, BTreeMap::new())
        .host_modules
        .insert("kv".into(), "./src/js/kv.mjs".into());
    let host = host_package(&tmp.join("host-edge"));
    graph
        .add("host-edge", &tmp.join("host-edge"), BTreeMap::new())
        .host = Some(host);
    graph
}

#[test]
fn host_modules_come_from_the_manifest_sections() {
    let wasm = module(&[
        (
            "velt.host",
            b"kv\tget\tstr\tasync:ref?\nvelt:worker\taccept\t\tasync:ref\n",
        ),
        ("other", b"x\ty\t\tvoid\n"),
        ("velt.host", b"kv\tput\tstr,rbytes\tvoid\napp\tf\t\tvoid\n"),
    ]);
    assert_eq!(
        imported_modules(&wasm).unwrap(),
        ["kv", "velt:worker", "app"]
    );
    assert_eq!(
        imported_modules(&module(&[])).unwrap(),
        Vec::<String>::new()
    );
    assert!(imported_modules(b"\0asm\x02").is_err());
    let mut cut = module(&[("velt.host", b"kv\tget\t\tvoid\n")]);
    cut.truncate(cut.len() - 3);
    assert!(imported_modules(&cut).is_err());
}

#[test]
fn a_host_package_is_resolved_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let g = graph(tmp.path());
    let host = resolve("host-edge", Some(&g), None, "My_App").unwrap();
    assert_eq!(
        (host.program.as_str(), host.package.as_str(), host.config.condition.as_str()),
        ("My_App", "host-edge", "edge")
    );
    assert!(resolve("host-edge", Some(&g), Some("wasm32-unknown-unknown"), "a").is_ok());
    for (name, target, packages, want) in [
        ("host-edge", Some("wasm32-wasip1"), Some(&g), "builds for `wasm32-unknown-unknown`: leave out `--target wasm32-wasip1`"),
        ("nope", None, Some(&g), "no package `nope` among the build's dependencies"),
        ("kv", None, Some(&g), "package `kv` is not a host package"),
        ("host-edge", None, None, "`--host host-edge` builds a package"),
    ] {
        let err = resolve(name, packages, target, "a").unwrap_err();
        assert!(err.contains(want), "{name}: {err}");
    }
    let mut native = graph(tmp.path());
    let wasi = HostConfig {
        target: "wasm32-wasip1".into(),
        ..host_package(&tmp.path().join("host-wasi"))
    };
    native.add("host-wasi", &tmp.path().join("host-wasi"), BTreeMap::new()).host = Some(wasi);
    let err = resolve("host-wasi", Some(&native), None, "a").unwrap_err();
    assert!(err.contains("which the JavaScript glue does not run"), "{err}");
}

#[test]
fn the_build_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let g = graph(tmp.path());
    let host = resolve("host-edge", Some(&g), None, "My_App").unwrap();
    let path = module_path(None, &tmp.path().join("target"), &host).unwrap();
    let dir = tmp.path().join("target/My_App-edge");
    assert_eq!(path, dir.join("program"));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("program.wasm");
    let section: &[u8] = b"kv\tget\tstr\tstr\nvelt:worker\taccept\t\tasync:ref\n";
    std::fs::write(&wasm, module(&[("velt.host", section)])).unwrap();
    write(&wasm, &host, Some(&g)).unwrap();
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    assert_eq!(
        read("velt_modules.mjs"),
        "// The host modules the program imports, by import module: written by `velt build \
         --host` (each build writes it again).\n\
         import m0 from \"./hosts/kv/js/kv.mjs\";\n\
         export const modules = { [\"kv\"]: m0 };\n"
    );
    assert!(read("velt_serve.mjs").contains("export function createServer"));
    assert_eq!(read("hosts/kv/js/kv.mjs"), "export default () => ({});\n");
    assert_eq!(read("entry.mjs"), "// the platform's entry\n");
    assert_eq!(read("edge.toml"), "name = \"my-app\"\n");
    assert!(read(MARKER).contains("velt build --host host-edge"));
    // An edited scaffolded file is kept; the entry is written again.
    std::fs::write(dir.join("edge.toml"), "name = \"mine\"\n").unwrap();
    std::fs::write(dir.join("entry.mjs"), "stale\n").unwrap();
    assert!(module_path(Some(&dir), tmp.path(), &host).is_ok());
    write(&wasm, &host, Some(&g)).unwrap();
    assert_eq!(read("edge.toml"), "name = \"mine\"\n");
    assert_eq!(read("entry.mjs"), "// the platform's entry\n");
    // A module no package provides (the build reports it first).
    let err = write(&wasm, &host, None).unwrap_err();
    assert!(err.contains("host module `kv`"), "{err}");
}

/// A build never writes over the user's files: only into a new, empty, scaffold-only or earlier
/// host build's directory.
#[test]
fn a_directory_of_the_user_s_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let g = graph(tmp.path());
    let host = resolve("host-edge", Some(&g), None, "app").unwrap();
    let dir = tmp.path().join("out");
    assert!(module_path(Some(&dir), tmp.path(), &host).is_ok());
    std::fs::create_dir_all(&dir).unwrap();
    assert!(module_path(Some(&dir), tmp.path(), &host).is_ok());
    std::fs::write(dir.join("edge.toml"), "mine\n").unwrap();
    assert!(module_path(Some(&dir), tmp.path(), &host).is_ok());
    std::fs::write(dir.join("entry.mjs"), "mine\n").unwrap();
    let err = module_path(Some(&dir), tmp.path(), &host).unwrap_err();
    assert!(
        err.contains("holds files a `--host` build did not write (`entry.mjs`…)"),
        "{err}"
    );
    std::fs::write(dir.join(MARKER), "").unwrap();
    assert!(module_path(Some(&dir), tmp.path(), &host).is_ok());
}

#[test]
fn a_module_named_proto_is_a_key() {
    let map = modules_map(&[("__proto__".into(), "hosts/p/p.mjs".into())]);
    assert!(map.contains("export const modules = { [\"__proto__\"]: m0 };"), "{map}");
    assert!(modules_map(&[]).ends_with("export const modules = {};\n"));
}

#[test]
fn program_names() {
    assert_eq!(program_name("My_App"), "my-app");
    assert_eq!(program_name("__"), "velt-app");
    assert_eq!(program_name(&"a".repeat(80)).len(), 63);
}
