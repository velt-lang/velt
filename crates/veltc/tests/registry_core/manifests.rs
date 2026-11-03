//! The `package.vlt` cases of tests/registry_core.rs: hand-written manifests (every field, valid
//! and not, and every form the Velt parser takes) and mutations of a valid one, token by token.

/// The opening every case starts from unless it replaces it.
pub const HEAD: &str =
    "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = ";

/// A manifest using every field once.
pub const FULL: &str = r##"{
  name: "hello",
  version: "0.1.0-rc.1+b5",
  velt: "0.5",
  description: "Says hello, in Ünïcode",
  keywords: ["greeting", "demo-2"],
  entry: "src/lib.vlt",
  registry: "https://registry.example",
  dependencies: {
    json: "^1.2",
    util: { path: "../util" },
    both: { path: "../both", version: ">=1, <2" },
    "@x/y": { path: "../y" },
    remote: { git: "https://github.com/a/b.git", tag: "v1.0", path: "pkgs/r" },
    pinned: { git: "git@github.com:a/b.git", rev: "0123456789abcdef0123456789abcdef01234567" },
    ws: "workspace",
  },
  paths: { "@app/*": "src/*", "~lib": "./src/lib.vlt" },
  imports: {
    "#io": { js: "./src/io/js.vlt", darwin: "./src/io/mac.ts", default: "./src/io/native.vlt" },
    "#one/two": "./src/two.tsx",
  },
  hostModules: { kv: "./src/kv.mjs", "my mod": "./src/m.js" },
  host: {
    target: "wasm32-unknown-unknown",
    condition: "edge-2",
    entry: "./src/entry.mjs",
    scaffold: { "wrangler.toml": "./src/wrangler.toml", "conf/a.json": "./src/a.json" },
  },
  jsx: { importSource: "sigx" },
  define: { __DEV__: "false", $V: "\"1.0\"", N: "12.5e3", S: "'prod'", Z: " null " },
  tsCompat: ["src/models", "src/Shared"],
  build: { cpu: "x86-64-v3" },
}"##;

/// Whole manifests: each is a case as written.
pub fn whole() -> Vec<String> {
    let mut out = vec![
        format!("{HEAD}{FULL};\n"),
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\" }}"),
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", native: {{ path: \"crate\", targets: [\"aarch64-apple-darwin\"] }} }}"),
        // The import is optional, its forms vary, and the statement end is `;` or a line break.
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" }".into(),
        "import { type Package } from 'velt/package'\nexport const pkg: Package = {name:'a',version:'1.0.0'}".into(),
        "import type { Package as P, Dependency } from \"velt/package\";export const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type { Package } from \"velt/package\" export const pkg: Package = { name: \"a\", version: \"1.0.0\" }".into(),
        "import { Package } from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type { Package } from \"velt/package\";\nimport type { Package } from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type {} from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type Package from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type { Package } from \"velt/pkg\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "import type { type Package } from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" };\nimport type { Package } from \"velt/package\";".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" };;".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" }; export const pkg: Package = { name: \"b\", version: \"1.0.0\" };".into(),
        "export const pkg = { name: \"a\", version: \"1.0.0\" };".into(),
        "export let pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "export const pkg: velt.Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "export const pkg: Package<string> = { name: \"a\", version: \"1.0.0\" };".into(),
        "export const manifest: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "\u{feff}export const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "#!/usr/bin/env velt\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\" };".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" }\n// the end".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" } /* unterminated".into(),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\" }\nas const;".into(),
        "export const pkg: Package = {\n  name: \"a\"\n  version: \"1.0.0\"\n};".into(),
        "".into(),
        "export const pkg: Package = \"a\";".into(),
        "export const pkg: Package = [];".into(),
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", x: 1 }};"),
    ];
    // Keys: names (keywords too), strings, numbers and `[literal]`.
    for key in [
        "name",
        "\"name\"",
        "'name'",
        "[\"name\"]",
        "['na\\x6de']",
        "\"na\\u006de\"",
        "\"n\\ame\"",
        "na\\u006de",
    ] {
        out.push(format!("{HEAD}{{ {key}: \"a\", version: \"1.0.0\" }};"));
    }
    for key in [
        "16",
        "0x10",
        "0X1_0",
        "0o20",
        "0b10000",
        "1.50",
        "1e3",
        "1E+3",
        ".5",
        "1_6",
        "16u8",
        "1.5f32",
        "0x",
        "0b2",
        "08",
        "1n",
        "[16]",
        "[-16]",
        "[-0]",
        "[0x10]",
        "[-\"a\"]",
        "[x]",
        "[Symbol.iterator]",
        "340282366920938463463374607431768211455",
        "340282366920938463463374607431768211456",
        "9007199254740993",
        "0xffffffffffffffffffffffffffffffff",
        "1e400",
        "0.1e-7",
        "123456789012345678901234567890",
    ] {
        out.push(format!(
            "{HEAD}{{ name: \"a\", version: \"1.0.0\", host: {{ target: \"wasm32-unknown-unknown\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: {{ {key}: \"./src/x\" }} }} }};"
        ));
    }
    // Strings: escapes, line continuations, and errors.
    for s in [
        "\"a\\nb\"",
        "\"\\t\"",
        "\"\\0\"",
        "\"\\01\"",
        "\"\\8\"",
        "\"\\x4\"",
        "\"\\u{1F600}\"",
        "\"\\uD83D\\uDE00\"",
        "\"\\uD83D\"",
        "\"\\uDE00\"",
        "\"\\u{110000}\"",
        "\"\\u{}\"",
        "\"\\q\"",
        "\"a\\\nb\"",
        "\"a\\\r\nb\"",
        "\"a\u{2028}b\"",
        "\"\\\u{2028}\"",
        "\"open",
        "'mixed\"",
        "\"\u{0}\"",
        "\"\\u{0000041}\"",
    ] {
        out.push(format!(
            "{HEAD}{{ name: \"a\", version: \"1.0.0\", description: {s} }};"
        ));
    }
    // What the owner listed (#1740): computed values, spreads, calls, comments in odd places,
    // Unicode names. (Comments after every token are among the mutations.)
    for body in [
        "name: \"a\", version: \"1.0.0\", ...{ description: \"x\" }",
        "name: \"a\", version: \"1.0.0\", keywords: [...[\"k\"]]",
        "name: \"a\", version: String(1)",
        "name: \"a\", version: \"1.0.0\", description: `x`",
        "name: \"a\", version: \"1.0.\" + \"0\"",
        "name: \"a\", version: \"1.0.0\", [\"desc\" + \"ription\"]: \"x\"",
        "name: \"a\", version: \"1.0.0\", description: (\"x\")",
        "name: \"a\", version: \"1.0.0\", description: \"x\" as string",
        "name: \"a\", version: \"1.0.0\", description: <string>\"x\"",
        "name: \"a\", version: \"1.0.0\", description: \"x\".trim()",
        "name: \"a\", version: \"1.0.0\", description: \"x\"[0]",
        "name: \"a\", version: \"1.0.0\", keywords: [\"k\",,]",
        "name: \"a\", version: \"1.0.0\", keywords: [,\"k\"]",
        "name: \"a\", version: \"1.0.0\", get description() { return \"x\" }",
        "name: \"a\", version: \"1.0.0\", description: /x/",
        "name: \"a\", version: \"1.0.0\", ñame: \"x\"",
        "name: \"ñ\", version: \"1.0.0\"",
        "name /* in */ : /* odd */ \"a\" /* places */ , version : \"1.0.0\" // end",
        "name: \"a\", version: \"1.0.0\", host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { \"ñ.toml\": \"./src/ñ\" } }",
        "name: \"a\", version: \"1.0.0\", hostModules: { \"ключ\": \"./src/a.mjs\" }",
        "name: \"a\", version: \"1.0.0\", dependencies: { \"ñ\": \"1\" }",
    ] {
        out.push(format!("{HEAD}{{ {body} }};"));
    }
    out.extend(review_cases());
    out.extend(fields());
    out
}

/// Cases from the review of the reader: nesting, number keys with a glued word, build files,
/// type annotations, import names, and the size limit.
fn review_cases() -> Vec<String> {
    let scaffold = |key: &str| {
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", host: {{ target: \"wasm32-unknown-unknown\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: {{ {key}: \"./src/x\" }} }} }};")
    };
    let mut out = vec![];
    for n in [3, 300, 20_000] {
        out.push(format!(
            "{HEAD}{{ name: \"a\", version: \"1.0.0\", keywords: {}{} }};",
            "[".repeat(n),
            "]".repeat(n)
        ));
        out.push(format!(
            "{HEAD}{{ name: \"a\", version: \"1.0.0\", x: {}1{} }};",
            "{a:".repeat(n),
            "}".repeat(n)
        ));
    }
    for key in [
        "1e",
        "1E",
        "1.5e",
        ".5e",
        "1_e",
        "1e_5",
        "1E_5",
        "[1e]",
        "[-1e]",
        "[-1E_5]",
        "1_e5",
        "0x1e",
        "1e5_",
        "1_.5",
        "0x1_f",
        "1e+",
        "1e-5",
        "1i32",
        "1.5u8",
        "0b1u8",
        "hosts",
        "\"hosts/a\"",
        "HOSTS",
        "\".velt-host\"",
        "\".VELT-HOST/x\"",
    ] {
        out.push(scaffold(key));
    }
    let body = "{ name: \"a\", version: \"1.0.0\" };";
    for ty in [
        "(Package)",
        "((Package))",
        "| Package",
        "& Package",
        "| & Package",
        "& | Package",
        "(| Package)",
        "| (Package)",
        "Package<>",
        "Package<\n>",
        "Package</*c*/>",
        "(Package)<>",
        "Package\n<>",
        "Package<x>",
        "Package[]",
        "Package | Package",
        "(Package",
        "Package)",
    ] {
        out.push(format!("export const pkg: {ty} = {body}"));
    }
    for names in [
        "type { type }",
        "type { type, Package }",
        "type { if }",
        "type { Package as class }",
        "{ type null }",
        "type { eval }",
        "type { Package as arguments }",
        "type { default }",
        "type { in }",
        "{ type from }",
        "{ type as }",
        "{ type as as as }",
        "{ type Package as P }",
        "type { Package, }",
        "type { Package, Package }",
        "{ type }",
        "{ type, Package }",
        "type { Package as }",
    ] {
        out.push(format!(
            "import {names} from \"velt/package\";\nexport const pkg: Package = {body}"
        ));
    }
    // The size limit, 65,536 bytes, with a multi-byte character at the edge of a comment.
    let base = format!("export const pkg: Package = {body}\n//");
    for total in [65_535usize, 65_536, 65_537] {
        let pad = total - base.len();
        out.push(format!("{base}{}", "x".repeat(pad)));
        if pad >= 2 {
            out.push(format!("{base}{}é", "x".repeat(pad - 2)));
        }
    }
    out
}

/// One field at a time, valid and not.
fn fields() -> Vec<String> {
    let base = |extra: &str| format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", {extra} }};");
    let mut out = vec![];
    for name in [
        "\"a\"", "\"@s/n\"", "\"std\"", "\"rt-x\"", "\"A\"", "\"@x\"", "1", "[\"a\"]",
    ] {
        out.push(format!("{HEAD}{{ name: {name}, version: \"1.0.0\" }};"));
    }
    for v in ["\"1.0.0\"", "\"1.0\"", "\"v1.0.0\"", "\"1.0.0-01\"", "true"] {
        out.push(format!("{HEAD}{{ name: \"a\", version: {v} }};"));
    }
    for extra in [
        "velt: \"0.5\"", "velt: \">=0.4, <0.6\"", "velt: \"\"", "velt: \" 0.5 \"", "velt: \"latest\"", "velt: \"*\"",
        "description: \"\"", "description: \" x\"", "description: \"a\\tb\"", "description: \"a\\u200Bb\"",
        "description: \"a\\u00A0\"", "keywords: []", "keywords: [\"a\", \"a\"]", "keywords: [\"A\"]",
        "keywords: [\"-a\"]", "keywords: [\"1\", \"2\", \"3\", \"4\", \"5\", \"6\", \"7\", \"8\", \"9\", \"10\", \"11\"]",
        "keywords: \"a\"", "keywords: [1]", "entry: \"../x.vlt\"", "entry: \"./x.vlt\"", "entry: \".\"", "entry: \"c:/x\"",
        "registry: \"ftp://x\"", "registry: \"http://x\"",
        "dependencies: { b: \"1\" }", "dependencies: { b: \">=1 <2\" }", "dependencies: { b: \"1.2.*\" }",
        "dependencies: { b: \"*\" }", "dependencies: { b: \"x, 1\" }", "dependencies: { b: \"1.*.3\" }",
        "dependencies: { b: \"~1.2.3-rc.1+b\" }", "dependencies: { b: \"=01\" }", "dependencies: { b: \"\" }",
        "dependencies: { b: { version: \"workspace\" } }", "dependencies: { b: {} }", "dependencies: { \"@s/b\": \"1\" }",
        "dependencies: { b: { git: \"https://u:p@h/r\" } }", "dependencies: { b: { git: \"ssh://git@h/r\", branch: \"main\" } }",
        "dependencies: { b: { git: \"https://h/r\", branch: \"a\", tag: \"b\" } }", "dependencies: { b: { tag: \"v1\", path: \"../b\" } }",
        "dependencies: { b: { git: \"https://h/r\", rev: \"ABC\" } }", "dependencies: { b: { git: \"https://h/r?x\" } }",
        "dependencies: { b: { git: \"https://h/r\", path: \"../x\" } }", "dependencies: { b: { path: 1 } }",
        "dependencies: { b: { url: \"x\" } }", "dependencies: { b: 1 }", "dependencies: { B: \"1\" }",
        "paths: { \"*\": \"src/*\" }", "paths: { \"@a/*\": \"src\" }", "paths: { \"@a\": \"../src\" }",
        "paths: { \"velt/x\": \"src/x\" }", "paths: { \"@a/**\": \"src/*\" }", "paths: { \"@a\": \"/abs\" }",
        "imports: { io: \"./src/a.vlt\" }", "imports: { \"#a/*\": \"./src/a.vlt\" }", "imports: { \"#a\": \"src/a.vlt\" }",
        "imports: { \"#a\": \"./lib/a.vlt\" }", "imports: { \"#a\": \"./src/a.d.ts\" }", "imports: { \"#a\": {} }",
        "imports: { \"#a\": { default: \"./src/a.vlt\", js: \"./src/b.vlt\" } }", "imports: { \"#a\": { Edge: \"./src/a.vlt\" } }",
        "imports: { \"#a\": { edge: \"./src/./a.vlt\" } }", "imports: { \"#a\": { edge: \"./src//a.vlt\" } }",
        "hostModules: { velt: \"./src/a.mjs\" }", "hostModules: { \"velt:x\": \"./src/a.mjs\" }", "hostModules: { __proto__: \"./src/a.mjs\" }",
        "hostModules: { \"\": \"./src/a.mjs\" }", "hostModules: { a: \"./src/a.ts\" }", "hostModules: { a: \"./lib/a.mjs\" }",
        "host: { target: \"wasm32\", condition: \"edge\", entry: \"./src/e.mjs\" }",
        "host: { target: \"a-b\", condition: \"js\", entry: \"./src/e.mjs\" }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/velt_web.mjs\" }",
        "host: { target: \"a-b\", condition: \"edge\" }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { \"e.mjs\": \"./src/x\" } }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { \"PROGRAM.WASM\": \"./src/x\" } }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { \"a//b\": \"./src/x\" } }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { a: \"src/x\" } }",
        "host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", port: 1 }",
        "jsx: {}", "jsx: { importSource: \"x/\" }", "jsx: { runtime: \"x\" }",
        "define: { \"a-b\": \"1\" }", "define: { A: \"1.\" }", "define: { A: \"1.e5\" }", "define: { A: \"1e\" }",
        "define: { A: \"0x10\" }", "define: { A: \"\\\"\\\\q\\\"\" }", "define: { A: \"\\\"\\\\ud800\\\"\" }",
        "define: { A: \"'a\\\\b'\" }", "define: { A: \"''\" }", "define: { A: \"'\" }", "define: { A: \"1e400\" }",
        "define: { A: \"-1\" }", "define: { A: \"undefined\" }",
        "native: {}", "native: { wasm: true }", "native: { wasm: false }", "native: { path: \"src\" }",
        "native: { path: \".hidden\" }", "native: { targets: [\"riscv64gc-unknown-linux-gnu\"] }", "native: { targets: \"x\" }",
        "tsCompat: []", "tsCompat: [\"src\", \"src/a\"]", "tsCompat: [\"src/a\", \"SRC/A\"]", "tsCompat: [\"src/a\", \"src\"]",
        "tsCompat: [\"/src\"]", "tsCompat: [\"src/\"]",
        "build: {}", "build: { cpu: \"Native\" }", "build: { opt: 3 }",
    ] {
        out.push(base(extra));
    }
    out.push(format!(
        "{HEAD}{{ name: \"@s/a\", version: \"1.0.0\", native: {{}} }};"
    ));
    out
}

/// What the mutations insert after a token.
const SNIPPETS: &[&str] = &[
    ",", ";", "/*c*/", "\n", "(", ")", "-", "0", "\"x\"", ":", "{}", "[]", "+", ".", "!", "?",
    "//x\n",
];

/// The tokens of `src`, roughly: enough to cut it at every boundary the Velt lexer sees.
fn boundaries(src: &str) -> Vec<usize> {
    let bytes = src.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        if b == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            i += 1;
        } else if b.is_ascii_alphanumeric() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else {
            i += 1;
        }
        if !src.is_char_boundary(i.min(src.len())) {
            continue;
        }
        if !bytes[start].is_ascii_whitespace() {
            out.push(start);
        }
    }
    out.push(src.len());
    out
}

/// Mutations of small valid manifests: each token deleted, doubled, and each snippet inserted
/// after it.
pub fn mutations() -> Vec<String> {
    let mut out = vec![];
    for src in [
        "import type { Package } from \"velt/package\";\nexport const pkg: Package = { name: \"a\", version: \"1.0.0\", keywords: [\"k\"], native: { wasm: false } };\n",
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\", host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { 7: \"./src/x\", [y]: \"./src/y\" } } }",
    ] {
        mutate(src, &mut out);
    }
    out
}

fn mutate(src: &str, out: &mut Vec<String>) {
    let cuts = boundaries(src);
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push(format!("{}{}", &src[..a], &src[b..]));
        out.push(format!("{}{}", &src[..b], &src[a..]));
        for snippet in SNIPPETS {
            out.push(format!("{}{snippet}{}", &src[..b], &src[b..]));
        }
    }
}
