use super::*;

const FILE: FileId = FileId(0);

fn read(src: &str) -> Manifest {
    Manifest::read(FILE, src).unwrap_or_else(|diags| {
        let messages: Vec<_> = diags.iter().map(|d| d.message.clone()).collect();
        panic!("unexpected errors {messages:?} in:\n{src}")
    })
}

/// Every error as (message, the source text its primary span covers).
fn errors(src: &str) -> Vec<(String, String)> {
    let diags = Manifest::read(FILE, src).expect_err("the manifest should be rejected");
    diags
        .iter()
        .map(|d| {
            let span = d.labels[0].span;
            (
                d.message.clone(),
                src[span.lo as usize..span.hi as usize].to_string(),
            )
        })
        .collect()
}

/// The only error of `src`, as (message, covered text).
fn error(src: &str) -> (String, String) {
    let mut all = errors(src);
    assert_eq!(all.len(), 1, "expected one error, got {all:?}");
    all.remove(0)
}

const HEAD: &str = "import type { Package } from \"velt:package\";\n";

/// A manifest with `fields` after a valid name and version.
fn with(fields: &str) -> String {
    format!(
        "{HEAD}export const pkg: Package = {{ name: \"app\", version: \"1.0.0\", {fields} }};\n"
    )
}

#[test]
fn full_manifest_matches_the_toml_one() {
    let m = read(
        r#"
        import type { Package } from "velt:package";

        // The package.
        export const pkg: Package = {
          name: "hello",
          version: "0.1.0",
          entry: 'src/app.vlt',
          registry: "https://registry.example.com",
          dependencies: {
            json: "1.2",
            util: { path: "../util" },
            http: { version: "0.3" }, /* trailing comma */
          },
          paths: { "@app/*": "src/*", "@cfg": "src/config" },
          jsx: { importSource: "sigx" },
          native: { path: "rust", targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"], wasm: false },
        };
        "#,
    );
    let toml = crate::manifest::legacy::from_toml(
        r#"
        registry = "https://registry.example.com"

        [package]
        name = "hello"
        version = "0.1.0"
        entry = "src/app.vlt"

        [dependencies]
        json = "1.2"
        util = { path = "../util" }
        http = { version = "0.3" }

        [paths]
        "@app/*" = "src/*"
        "@cfg" = "src/config"

        [jsx]
        importSource = "sigx"

        [native]
        path = "rust"
        targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
        wasm = false
        "#,
    )
    .unwrap();
    assert_eq!(m, toml);
    assert!(m.native.is_some());
}

#[test]
fn minimal_manifest_uses_defaults() {
    let m = read("export const pkg: Package = { name: \"app\", version: \"1.0.0\" };");
    assert_eq!(m.package.entry, "src/main.vlt");
    assert_eq!(m.registry, None);
    assert!(m.dependencies.is_empty() && m.paths.is_empty());
    assert_eq!(m.jsx, None);
    assert_eq!(m.native, None);
    assert_eq!(read(&with("jsx: {}")).jsx, Some(JsxConfig::default()));
}

#[test]
fn only_data_is_allowed() {
    let cases = [
        (
            "registry: null",
            "`null` is not allowed; leave the key out",
            "null",
        ),
        ("entry: env(\"E\")", "calls are not allowed", "env(\"E\")"),
        (
            "entry: `src/${x}`",
            "template literals are not allowed",
            "`src/${x}`",
        ),
        ("entry: main", "names are not allowed", "main"),
        (
            "entry: \"a\" + \"b\"",
            "operators are not allowed",
            "\"a\" + \"b\"",
        ),
        (
            "entry: \"a\" as string",
            "`as` is not allowed",
            "\"a\" as string",
        ),
        ("entry: new Main()", "`new` is not allowed", "new Main()"),
        ("paths: { ...base }", "spreads are not allowed", "base"),
        ("paths: [...base]", "spreads are not allowed", "...base"),
        (
            "paths: { entry }",
            "shorthand properties are not allowed",
            "entry",
        ),
    ];
    for (fields, message, text) in cases {
        let (got, covered) = error(&with(fields));
        assert!(got.contains(message), "{fields}: {got}");
        assert_eq!(covered, text, "{fields}");
    }
}

#[test]
fn the_file_holds_one_typed_pkg_constant() {
    let body = "{ name: \"app\", version: \"1.0.0\" }";
    for src in [
        String::new(),
        format!("const pkg: Package = {body};"),
        format!("export let pkg: Package = {body};"),
        format!("export const manifest: Package = {body};"),
        format!("export const pkg = {body};"),
        format!("export const pkg: Other = {body};"),
        format!("export const pkg: Package = {body};\nexport const extra: Package = {body};"),
        format!("export const pkg: Package = {body};\nfunction f(): void {{}}"),
    ] {
        let (message, _) = error(&src);
        assert_eq!(message, SHAPE, "{src}");
    }
    for import in [
        "import { Package } from \"velt:package\";",
        "import type { Package } from \"./types\";",
        "import * as p from \"velt:package\";",
    ] {
        let (message, covered) = error(&format!("{import}\nexport const pkg: Package = {body};"));
        assert!(
            message.contains("may only import types from `velt:package`"),
            "{message}"
        );
        assert_eq!(covered, import);
    }
}

#[test]
fn keys_are_checked() {
    let all = errors(&with("dependecies: {}"));
    assert_eq!(
        all,
        [(
            "unknown key `dependecies` in the manifest".into(),
            "dependecies".into()
        )]
    );
    let diags = Manifest::read(FILE, &with("dependecies: {}")).unwrap_err();
    assert_eq!(diags[0].notes, ["did you mean `dependencies`?"]);

    let (message, covered) = error(&with("jsx: { import_source: \"x\" }"));
    assert_eq!(message, "unknown key `import_source` in the manifest");
    assert_eq!(covered, "import_source");
    let (message, _) = error(&with(
        "dependencies: { util: { path: \"../u\", git: \"x\" } }",
    ));
    assert_eq!(message, "unknown key `git` in the manifest");

    let (message, covered) = error(&with("entry: \"a\", entry: \"b\""));
    assert_eq!(message, "duplicate key `entry`");
    assert_eq!(covered, "entry");

    let (message, _) = error("export const pkg: Package = { version: \"1.0.0\" };");
    assert_eq!(message, "the manifest is missing `name`");
}

#[test]
fn values_have_the_right_kind() {
    let (message, covered) = error(&with("entry: 1"));
    assert_eq!(message, "`entry` must be a string, not a number");
    assert_eq!(covered, "1");
    let (message, _) = error(&with("paths: [\"src\"]"));
    assert_eq!(message, "`paths` must be an object, not an array");
    let (message, _) = error(&with("dependencies: { util: true }"));
    assert!(message.contains("must be a version requirement or an object, not a boolean"));
    let (message, _) = error("export const pkg: Package = \"app\";");
    assert_eq!(message, "the manifest must be an object, not a string");
}

#[test]
fn field_rules_point_at_the_value() {
    let src =
        format!("{HEAD}export const pkg: Package = {{ name: \"Bad Name\", version: \"1\" }};");
    let all = errors(&src);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[0].0.starts_with("invalid package name `Bad Name`"));
    assert_eq!(all[0].1, "\"Bad Name\"");
    assert!(all[1].0.starts_with("version `1` is not a semver version"));
    assert_eq!(all[1].1, "\"1\"");

    let (message, covered) = error(&with("paths: { \"@app/*\": \"../x/*\" }"));
    assert!(message.starts_with("alias `@app/*`:"), "{message}");
    assert_eq!(covered, "\"@app/*\": \"../x/*\"");

    let (message, covered) = error(&with("dependencies: { y: {} }"));
    assert_eq!(message, "dependency `y` needs a `version` or a `path`");
    assert_eq!(covered, "{}");
    let (message, _) = error(&with("dependencies: { y: \"one\" }"));
    assert!(
        message.contains("invalid version requirement `one`"),
        "{message}"
    );
    let (message, covered) = error(&with("dependencies: { Y: \"1\" }"));
    assert_eq!(
        message,
        "invalid dependency name `Y` (lowercase letters and digits, starting with a letter, with single `-` or `_` between words)"
    );
    assert_eq!(covered, "Y");
    let (message, _) = error(&with("registry: \"ftp://x\""));
    assert!(
        message.contains("must be an http:// or https:// URL"),
        "{message}"
    );
    let (message, _) = error(&with("jsx: { importSource: \"ui/\" }"));
    assert!(message.contains("is not a module specifier"), "{message}");
}

#[test]
fn the_toolchain_requirement() {
    for req in ["0.1", "0.1.3", "=0.1.3", ">=0.1, <0.3"] {
        let m = read(&with(&format!("velt: \"{req}\"")));
        assert_eq!(m.toolchain.as_deref(), Some(req));
        assert_eq!(m.to_json()["velt"], req);
        assert_eq!(
            Manifest::read(FILE, &m.to_vlt()).unwrap(),
            m,
            "round trip of {req}"
        );
    }
    let without = read(&with("entry: \"src/a.vlt\""));
    assert_eq!(without.toolchain, None);
    assert!(without.to_json().get("velt").is_none());
    let (message, covered) = error(&with("velt: \"latest\""));
    assert!(
        message.starts_with("velt `latest` is not a version requirement"),
        "{message}"
    );
    assert_eq!(covered, "\"latest\"");
    let (message, _) = error(&with("velt: 0.1"));
    assert_eq!(message, "`velt` must be a string, not a number");
}

#[test]
fn syntax_errors_are_reported_as_is() {
    let diags = Manifest::read(FILE, "export const pkg: Package = { name: };").unwrap_err();
    assert!(!diags.is_empty());
}

// Hostile input: manifests also come from uploaded archives (the registry server).

#[test]
fn oversized_manifest_is_refused_before_parsing() {
    let src = with(&format!("/*{}*/", "x".repeat(MAX_BYTES)));
    let (message, _) = error(&src);
    assert_eq!(message, "the manifest is larger than 64 KiB");
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    for depth in [300, 30_000] {
        let src = with(&format!(
            "paths: {}{}",
            "[".repeat(depth),
            "]".repeat(depth)
        ));
        assert!(src.len() <= MAX_BYTES);
        assert!(Manifest::read(FILE, &src).is_err());
    }
}

#[test]
fn too_many_values_are_refused() {
    let array = with(&format!("paths: [{}]", "0,".repeat(MAX_VALUES)));
    let many_keys: String = (0..MAX_VALUES / 2).map(|i| format!("k{i}:[0],")).collect();
    let keys = with(&format!("paths: {{ {many_keys} }}"));
    for src in [array, keys] {
        assert!(src.len() <= MAX_BYTES, "{}", src.len());
        let (message, _) = error(&src);
        assert_eq!(message, "the manifest has more than 10000 values");
    }
}

#[test]
fn edit_distance_counts_edits() {
    assert_eq!(edit_distance("dependencies", "dependecies"), 1);
    assert_eq!(edit_distance("jsx", "jsx"), 0);
    assert_eq!(edit_distance("", "abc"), 3);
}

#[test]
fn imports_name_at_least_one_type() {
    let body = "export const pkg: Package = { name: \"app\", version: \"1.0.0\" };";
    for import in [
        "import \"velt:package\";",
        "import type {} from \"velt:package\";",
    ] {
        let (message, covered) = error(&format!("{import}\n{body}"));
        assert!(
            message.contains("may only import types from `velt:package`"),
            "{import}: {message}"
        );
        assert_eq!(covered, import);
    }
    let second = "import type { Package } from \"velt:package\";";
    let (message, covered) = error(&format!("{HEAD}{second}\n{body}"));
    assert_eq!(message, "`package.vlt` may have at most one import");
    assert_eq!(covered, second);
}

#[test]
fn regular_expressions_are_named() {
    let (message, covered) = error(&with("entry: /a/g"));
    assert_eq!(
        message,
        "the manifest is data only: regular expressions are not allowed"
    );
    assert_eq!(covered, "/a/g");
    let (message, _) = error(&with("entry: new RegExp(\"a\", \"\")"));
    assert_eq!(message, "the manifest is data only: `new` is not allowed");
}

#[test]
fn did_you_mean_needs_a_close_key() {
    let note = |fields: &str| {
        Manifest::read(FILE, &with(fields)).unwrap_err()[0]
            .notes
            .clone()
    };
    assert!(note("x: 1").is_empty());
    assert!(note("jsc: {}").contains(&"did you mean `jsx`?".to_string()));
    assert!(note("regsitry: \"\"").contains(&"did you mean `registry`?".to_string()));
}

#[test]
fn many_distinct_keys_are_read() {
    let keys: String = (0..4_000).map(|i| format!("\"@k{i}\":\"s\",")).collect();
    let src = with(&format!("paths: {{ {keys} }}"));
    assert!(src.len() <= MAX_BYTES);
    assert_eq!(read(&src).paths.len(), 4_000);
}

#[test]
fn native_object_matches_the_toml_table() {
    let m = read(&with("native: { targets: [\"x86_64-pc-windows-msvc\"] }"));
    let native = m.native.expect("native is decoded");
    assert_eq!(native.path, "native");
    assert_eq!(native.targets, ["x86_64-pc-windows-msvc"]);
    assert!(!native.wasm);
    let empty = read(&with("native: {}")).native.expect("native is decoded");
    let toml = crate::manifest::legacy::from_toml(
        "[package]
name = \"app\"
version = \"1.0.0\"
[native]
",
    )
    .unwrap()
    .native
    .unwrap();
    assert_eq!(empty, toml);
}

#[test]
fn native_keys_are_checked() {
    let (message, covered) = error(&with("native: { target: [] }"));
    assert_eq!(message, "unknown key `target` in the manifest");
    assert_eq!(covered, "target");
    let diags = Manifest::read(FILE, &with("native: { target: [] }")).unwrap_err();
    assert_eq!(diags[0].notes, ["did you mean `targets`?"]);
    let diags = Manifest::read(FILE, &with("natve: {}")).unwrap_err();
    assert_eq!(diags[0].notes, ["did you mean `native`?"]);
}

#[test]
fn native_rules_point_at_the_value() {
    let (message, covered) = error(&with(
        "native: { targets: [\"x86_64-unknown-linux-gnu\", \"sparc-sun-solaris\"] }",
    ));
    assert!(
        message.starts_with("target `sparc-sun-solaris` is not supported (supported: "),
        "{message}"
    );
    assert_eq!(covered, "\"sparc-sun-solaris\"");
    for path in ["../x", "src", "target", ""] {
        let (message, covered) = error(&with(&format!("native: {{ path: \"{path}\" }}")));
        assert!(
            message.contains("must be the name of a directory in the package root"),
            "{path}: {message}"
        );
        assert_eq!(covered, format!("\"{path}\""));
    }
    let (message, covered) = error(&with("native: { wasm: true }"));
    assert_eq!(
        message,
        "`wasm: true` is not supported yet: packages with native code cannot target WebAssembly"
    );
    assert_eq!(covered, "true");
}

#[test]
fn native_values_have_the_right_kind() {
    let cases = [
        (
            "native: true",
            "`native` must be an object, not a boolean",
            "true",
        ),
        (
            "native: { path: 1 }",
            "`path` must be a string, not a number",
            "1",
        ),
        (
            "native: { targets: \"x86_64-pc-windows-msvc\" }",
            "`targets` must be an array, not a string",
            "\"x86_64-pc-windows-msvc\"",
        ),
        (
            "native: { targets: [1] }",
            "`targets` entries must be strings, not a number",
            "1",
        ),
        (
            "native: { wasm: \"no\" }",
            "`wasm` must be a boolean, not a string",
            "\"no\"",
        ),
    ];
    for (fields, message, text) in cases {
        let (got, covered) = error(&with(fields));
        assert_eq!(got, message, "{fields}");
        assert_eq!(covered, text, "{fields}");
    }
}

#[test]
fn description_and_keywords() {
    let m = read(&with(
        "description: \"Fast JSON for Velt\", keywords: [\"json\", \"parser\", \"x86-64\"]",
    ));
    assert_eq!(m.package.description.as_deref(), Some("Fast JSON for Velt"));
    assert_eq!(m.package.keywords, ["json", "parser", "x86-64"]);
    // Round trip through the writer, which puts them after `version`.
    let text = m.to_vlt();
    assert!(text.contains("version: \"1.0.0\",\n  description: \"Fast JSON for Velt\",\n  keywords: [\"json\", \"parser\", \"x86-64\"],"), "{text}");
    assert_eq!(read(&text), m);
    let json = m.to_json();
    assert_eq!(json["description"], "Fast JSON for Velt");
    assert_eq!(json["keywords"][1], "parser");

    let long = "x".repeat(301);
    let cases = [
        ("description: \"\"", "`description` is empty; remove the field instead", "\"\""),
        (&*format!("description: \"{long}\""), "`description` has 301 characters; at most 300 are allowed (`velt search` shows the start of a long one)", &*format!("\"{long}\"")),
        ("description: \"a\\nb\"", "`description` must be one line (no line breaks, tabs or other control characters)", "\"a\\nb\""),
        ("description: \" a\"", "`description` starts or ends with whitespace", "\" a\""),
        ("keywords: []", "`keywords` is empty; remove the field instead", "[]"),
        ("keywords: [\"Json\"]", "keyword `Json` must be lowercase letters, digits and `-`, starting with a letter or digit (at most 32 characters)", "\"Json\""),
        ("keywords: [\"-x\"]", "keyword `-x` must be lowercase letters, digits and `-`, starting with a letter or digit (at most 32 characters)", "\"-x\""),
        ("keywords: [\"a\", \"a\"]", "duplicate keyword `a`", "\"a\""),
        ("keywords: \"json\"", "`keywords` must be an array, not a string", "\"json\""),
    ];
    for (fields, message, text) in cases {
        let (got, covered) = error(&with(fields));
        assert_eq!(got, message, "{fields}");
        assert_eq!(covered, text, "{fields}");
    }
    let eleven: Vec<String> = (0..11).map(|i| format!("\"k{i}\"")).collect();
    let (got, _) = error(&with(&format!("keywords: [{}]", eleven.join(", "))));
    assert_eq!(got, "`keywords` has 11 entries; at most 10 are allowed");
    let thirty_three = "k".repeat(33);
    let (got, _) = error(&with(&format!("keywords: [\"{thirty_three}\"]")));
    assert!(
        got.starts_with(&format!("keyword `{thirty_three}` must be")),
        "{got}"
    );
    // Characters that reorder or hide text (here a right-to-left override, a line separator).
    for c in ["\u{202E}", "\u{2028}", "\u{200B}"] {
        let (got, _) = error(&with(&format!("description: \"a{c}b\"")));
        assert!(got.contains("contains the invisible character U+"), "{got}");
    }
    // 300 characters that are not ASCII are fine: characters, not bytes, count.
    read(&with(&format!("description: \"{}\"", "é".repeat(300))));
}

#[test]
fn ts_compat_folders() {
    let m = read(&with(
        "tsCompat: [\"src/components\", \"src/models\", \"shared\"]",
    ));
    assert_eq!(m.ts_compat, ["src/components", "src/models", "shared"]);
    let text = m.to_vlt();
    assert!(
        text.contains("tsCompat: [\"src/components\", \"src/models\", \"shared\"]"),
        "{text}"
    );
    assert_eq!(read(&text), m);
    assert_eq!(m.to_json()["tsCompat"][1], "src/models");
    assert!(read(&with("")).to_json().get("tsCompat").is_none());
    let root = std::path::Path::new("pkg");
    assert_eq!(m.ts_compat_dirs(root)[0], root.join("src/components"));

    let not_inside = |dir: &str| {
        format!(
            "`tsCompat` folder `{dir}` must be a `/`-separated path inside the package, relative \
             to its root (such as `src/models`: no `.` or `..` parts, no leading or trailing `/`)"
        )
    };
    let cases = [
        (
            "tsCompat: []",
            "`tsCompat` is empty; remove the field instead".to_string(),
            "[]",
        ),
        (
            "tsCompat: \"src\"",
            "`tsCompat` must be an array, not a string".into(),
            "\"src\"",
        ),
        (
            "tsCompat: [1]",
            "`tsCompat` entries must be strings, not a number".into(),
            "1",
        ),
        ("tsCompat: [\"../x\"]", not_inside("../x"), "\"../x\""),
        ("tsCompat: [\"/abs\"]", not_inside("/abs"), "\"/abs\""),
        ("tsCompat: [\"src/\"]", not_inside("src/"), "\"src/\""),
        ("tsCompat: [\"./src\"]", not_inside("./src"), "\"./src\""),
        ("tsCompat: [\"\"]", not_inside(""), "\"\""),
        ("tsCompat: [\"C:/x\"]", not_inside("C:/x"), "\"C:/x\""),
        (
            "tsCompat: [\"src\\\\a\"]",
            not_inside("src\\a"),
            "\"src\\\\a\"",
        ),
        (
            "tsCompat: [\"src/a\", \"src/a\"]",
            "`tsCompat` lists `src/a` twice".into(),
            "\"src/a\"",
        ),
        (
            "tsCompat: [\"src\", \"src/a\"]",
            "`src/a` is inside `src`, which `tsCompat` already lists".into(),
            "\"src/a\"",
        ),
        (
            "tsCompat: [\"src/a/b\", \"src/a\"]",
            "`src/a` contains `src/a/b`, which `tsCompat` already lists; keep one of them".into(),
            "\"src/a\"",
        ),
        // Case is ignored: these are one folder on case-insensitive file systems.
        (
            "tsCompat: [\"src/models\", \"src/Models/sub\"]",
            "`src/Models/sub` is inside `src/models`, which `tsCompat` already lists".into(),
            "\"src/Models/sub\"",
        ),
        (
            "tsCompat: [\"src/Models\", \"src/models\"]",
            "`tsCompat` lists `src/models` and `src/Models`, which differ only in case (the same \
             folder on case-insensitive file systems); keep one of them"
                .into(),
            "\"src/models\"",
        ),
    ];
    for (fields, message, text) in cases {
        let (got, covered) = error(&with(fields));
        assert_eq!(got, message, "{fields}");
        assert_eq!(covered, text, "{fields}");
    }
    // Siblings sharing a prefix are different folders.
    read(&with("tsCompat: [\"src/a\", \"src/ab\"]"));
}

#[test]
fn missing_ts_compat_folders_are_warnings_at_their_string() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/models")).unwrap();
    std::fs::write(tmp.path().join("src/file.ts"), "").unwrap();
    let src = with("tsCompat: [\"src/models\", \"src/gone\", \"src/file.ts\"]");
    let warnings = missing_ts_compat_dirs(FILE, &src, tmp.path());
    let got: Vec<(String, &str)> = warnings
        .iter()
        .map(|d| {
            assert_eq!(d.severity, velt_common::Severity::Warning);
            let span = d.labels[0].span;
            (d.message.clone(), &src[span.lo as usize..span.hi as usize])
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "`tsCompat` folder `src/gone` does not exist".to_string(),
                "\"src/gone\""
            ),
            (
                "`tsCompat` folder `src/file.ts` is not a folder".to_string(),
                "\"src/file.ts\""
            ),
        ]
    );
    // An unreadable manifest is the reader's to report.
    assert!(missing_ts_compat_dirs(FILE, "export const", tmp.path()).is_empty());
}
