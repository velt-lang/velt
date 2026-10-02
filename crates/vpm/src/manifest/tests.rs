use super::*;

const FULL: &str = r#"
import type { Package } from "velt:package";

export const pkg: Package = {
  name: "hello",
  version: "0.1.0",
  entry: "src/app.vlt",
  registry: "https://registry.example.com",
  dependencies: { json: "1.2", "my-util": { path: "../util" }, http: { version: "0.3" } },
  paths: { "@app/*": "src/*" },
  jsx: { importSource: "sigx" },
  native: { path: "rs", targets: ["x86_64-unknown-linux-gnu"] },
};
"#;

#[test]
fn to_vlt_round_trips_and_is_formatted() {
    let m = Manifest::parse(FULL).unwrap();
    let text = m.to_vlt();
    assert!(
        text.starts_with("import type { Package } from \"velt:package\";\n"),
        "{text}"
    );
    assert!(
        text.contains("\"my-util\": { path: \"../util\" }"),
        "{text}"
    );
    assert_eq!(Manifest::parse(&text).unwrap(), m);
    assert_eq!(velt_fmt::format_source(&text).unwrap(), text);

    // Defaults are left out.
    let minimal =
        Manifest::parse("export const pkg: Package = { name: \"a\", version: \"1.0.0\" };")
            .unwrap()
            .to_vlt();
    assert_eq!(
        minimal,
        "import type { Package } from \"velt:package\";\n\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };\n"
    );
}

#[test]
fn strings_are_escaped() {
    let mut m = Manifest::parse(FULL).unwrap();
    let path = "../a \"b\"\\c\u{1}\n";
    m.dependencies.insert(
        "odd".into(),
        Dependency::Detailed(DetailedDependency {
            version: None,
            path: Some(path.into()),
        }),
    );
    let text = m.to_vlt();
    assert_eq!(
        Manifest::parse(&text).unwrap().dependencies["odd"].path(),
        Some(path)
    );
}

#[test]
fn json_has_the_package_shape_with_defaults() {
    let json = Manifest::parse(FULL).unwrap().to_json();
    assert_eq!(json["name"], "hello");
    assert_eq!(json["dependencies"]["json"], "1.2");
    assert_eq!(json["dependencies"]["my-util"]["path"], "../util");
    assert_eq!(json["paths"]["@app/*"], "src/*");
    assert_eq!(json["jsx"]["importSource"], "sigx");
    assert_eq!(json["native"]["wasm"], false);
    let minimal =
        Manifest::parse("export const pkg: Package = { name: \"a\", version: \"1.0.0\" };")
            .unwrap()
            .to_json();
    assert_eq!(minimal["entry"], DEFAULT_ENTRY);
    assert_eq!(minimal["dependencies"], serde_json::json!({}));
    assert!(minimal.get("native").is_none() && minimal.get("registry").is_none());
}

#[test]
fn errors_name_the_file_and_line() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(MANIFEST_FILE);
    std::fs::write(
        &path,
        "export const pkg: Package = {\n  name: \"a\",\n  version: 1,\n};\n",
    )
    .unwrap();
    let err = Manifest::from_path(&path).unwrap_err();
    assert_eq!(
        err,
        format!(
            "{}:3:12: error: `version` must be a string, not a number",
            path.display()
        )
    );
    let err = Manifest::parse("export const pkg: Package = { name: null };").unwrap_err();
    assert!(
        err.starts_with("package.vlt:1:37: error: `null` is not allowed"),
        "{err}"
    );
}

#[test]
fn reads_are_bounded_and_utf8() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(MANIFEST_FILE);
    std::fs::write(&path, vec![b' '; read::MAX_BYTES + 1]).unwrap();
    assert!(Manifest::from_path(&path)
        .unwrap_err()
        .ends_with("larger than 64 KiB"));
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert!(Manifest::from_path(&path)
        .unwrap_err()
        .ends_with("not valid UTF-8"));
}

#[test]
fn a_velt_toml_gets_the_converted_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(LEGACY_MANIFEST_FILE),
        "[package]\nname = \"old\"\nversion = \"1.0.0\"\n[dependencies]\njson = \"1\"\n",
    )
    .unwrap();
    let err = Manifest::from_dir(tmp.path()).unwrap_err();
    assert!(
        err.contains("velt.toml` is no longer read; the manifest is `package.vlt`"),
        "{err}"
    );
    let converted = &err[err.find("import type").expect("the converted manifest")..];
    let m = Manifest::parse(converted).unwrap();
    assert_eq!((m.package.name.as_str(), m.dependencies.len()), ("old", 1));

    std::fs::write(tmp.path().join(LEGACY_MANIFEST_FILE), "[package").unwrap();
    let err = Manifest::from_dir(tmp.path()).unwrap_err();
    assert!(err.contains("could not be converted"), "{err}");

    // Once package.vlt exists, velt.toml is ignored.
    std::fs::write(
        tmp.path().join(MANIFEST_FILE),
        Manifest::parse(converted).unwrap().to_vlt(),
    )
    .unwrap();
    assert!(Manifest::from_dir(tmp.path()).is_ok());
}

#[test]
fn entry_stays_inside_the_package() {
    for entry in [
        "",
        "/etc/passwd",
        "../../x.vlt",
        "src/../../x.vlt",
        "src\\\\main.vlt",
        "C:/x.vlt",
        "c:x.vlt",
        ".",
        "./",
        "src//main.vlt",
    ] {
        let src = format!(
            "export const pkg: Package = {{ name: \"a\", version: \"1.0.0\", entry: \"{entry}\" }};"
        );
        let err = Manifest::parse(&src).unwrap_err();
        assert!(
            err.contains("must be a `/`-separated path to a file inside the package"),
            "{entry}: {err}"
        );
    }
    let ok =
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\", entry: \"./main.vlt\" };";
    assert_eq!(Manifest::parse(ok).unwrap().package.entry, "./main.vlt");
}

#[test]
fn finds_enclosing_package() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("a/src/deep")).unwrap();
    std::fs::write(tmp.path().join("a/package.vlt"), "").unwrap();
    assert_eq!(
        find_package_root(&tmp.path().join("a/src/deep")),
        Some(tmp.path().join("a"))
    );
    assert_eq!(
        find_package_root(&tmp.path().join("a/src/deep/x.vlt")),
        Some(tmp.path().join("a"))
    );
    // A package that still has a velt.toml is found, so it can be told to migrate.
    std::fs::create_dir_all(tmp.path().join("b/src")).unwrap();
    std::fs::write(tmp.path().join("b/velt.toml"), "").unwrap();
    assert_eq!(
        find_package_root(&tmp.path().join("b/src")),
        Some(tmp.path().join("b"))
    );
}
