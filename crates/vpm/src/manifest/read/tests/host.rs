//! `host` of package.vlt (`crate::host`).

use super::*;

#[test]
fn host_packages_are_read_checked_and_written_back() {
    let m = read(&with(
        r#"host: { target: "wasm32-unknown-unknown", condition: "edge", entry: "./src/entry.mjs", scaffold: { "edge.toml": "./src/edge.toml" } }"#,
    ));
    let host = m.host.as_ref().expect("host");
    assert_eq!(
        (
            host.target.as_str(),
            host.condition.as_str(),
            host.entry.as_str()
        ),
        ("wasm32-unknown-unknown", "edge", "./src/entry.mjs")
    );
    assert_eq!(host.scaffold["edge.toml"], "./src/edge.toml");
    let again = Manifest::read(FILE, &m.to_vlt()).unwrap();
    assert_eq!(again.host, m.host);
    assert_eq!(m.to_json()["host"]["condition"], "edge");
    // A variant may name a host's condition.
    let m = read(&with(
        r##"imports: { "#io": { edge: "./src/a.vlt", default: "./src/b.vlt" } }"##,
    ));
    assert_eq!(m.imports["#io"][0].condition, "edge");
    let full = |extra: &str| {
        format!(
            r#"host: {{ target: "wasm32-unknown-unknown", condition: "edge", entry: "./src/e.mjs"{extra} }}"#
        )
    };
    for (fields, message, at) in [
        (
            r#"host: { target: "wasm32-unknown-unknown", condition: "js", entry: "./src/e.mjs" }"#
                .to_string(),
            "`js` is a built-in condition",
            "\"js\"",
        ),
        (
            r#"host: { target: "wasm32-unknown-unknown", condition: "edge", entry: "./e.mjs" }"#
                .to_string(),
            "below `./src/`",
            "\"./e.mjs\"",
        ),
        (
            full(r#", scaffold: { "../x.toml": "./src/x.toml" }"#),
            "inside the build's directory",
            "\"../x.toml\": \"./src/x.toml\"",
        ),
        (
            full(r#", scaffold: { "program.wasm": "./src/x" }"#),
            "the build writes itself",
            "\"program.wasm\": \"./src/x\"",
        ),
        (
            full(r#", wrangler: true"#),
            "unknown key `wrangler`",
            "wrangler",
        ),
        (
            r#"host: { target: "wasm32-unknown-unknown", entry: "./src/e.mjs" }"#.to_string(),
            "`host` needs `condition`",
            "{ target: \"wasm32-unknown-unknown\", entry: \"./src/e.mjs\" }",
        ),
    ] {
        let (msg, covered) = error(&with(&fields));
        assert!(msg.contains(message), "{fields}: {msg}");
        assert_eq!(covered, at, "{fields}");
    }
}
