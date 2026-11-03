//! Globals loaded on demand ([`super::globals`]): by a name a user module mentions, and the
//! conditional ones by their std module and a word.

use super::tests::{load, messages, paths, Tree};
use super::*;

#[test]
fn globals_load_when_a_user_module_names_them() {
    let t = Tree::new();
    let std = t.path("std");
    t.write("std/prelude/index.vlt", "import \"./core\";\n");
    t.write(
        "std/prelude/core.vlt",
        "export function id(): i64 { return 0; }\n",
    );
    t.write(
        "std/prelude/global/web.vlt",
        "export { fetch, Response } from \"velt/web\";\n",
    );
    t.write(
        "std/prelude/global/url.vlt",
        "export { URL } from \"velt/net/url\";\n",
    );
    // A std module that mentions a global's name does not load it.
    t.write(
        "std/web.vlt",
        "// URL\nexport function fetch() {}\nexport class Response {}\n",
    );
    t.write("std/net/url/index.vlt", "export class URL {}\n");
    t.write("std/server/index.vlt", "export class Response {}\n");
    let opts = || LoadOptions {
        std_root: Some(std.clone()),
        ..Default::default()
    };
    let plain = t.write("p/plain.vlt", "// prefetch\nfunction main() {}\n");
    let (l, diags, _) = load(&plain, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["std/prelude/core", "main"]);

    t.write(
        "p/lib.vlt",
        "export async function get() { await fetch(); }\n",
    );
    let root = t.write(
        "p/main.vlt",
        "import { get } from \"./lib\";\nfunction main() {}\n",
    );
    let (l, diags, _) = load(&root, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    let want = [
        "std/prelude/core",
        "main",
        "lib",
        "std/prelude/global/web",
        "std/web",
    ];
    assert_eq!(paths(&l), want);

    // Names a module binds itself (an import, a declaration) hide the global: nothing loads.
    let own = t.write(
        "p/own.vlt",
        "import { Response } from \"velt/server\";\nclass URL {}\n\
         function main() { const r = new Response(); const u = new URL(); }\n",
    );
    let (l, diags, _) = load(&own, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["std/prelude/core", "main", "std/server"]);
}

#[test]
fn a_conditional_global_loads_with_its_module_and_a_word() {
    let t = Tree::new();
    let std = t.path("std");
    t.write(
        "std/prelude/global/web.vlt",
        "export { Response } from \"velt/web\";\n",
    );
    t.write(
        "std/prelude/global/web_bytes.vlt",
        "// load when std/web uses: bytes arrayBuffer\nimport \"velt/web_bytes\";\n",
    );
    t.write("std/web.vlt", "export class Response {}\n");
    t.write(
        "std/web_bytes.vlt",
        "import { Response } from \"velt/web\";\nextend Response {\n  bytes(): i64 { return 1; }\n}\n",
    );
    let opts = || LoadOptions {
        std_root: Some(std.clone()),
        ..Default::default()
    };
    // The module and a word: loaded, after the rest.
    let both = t.write(
        "p/both.vlt",
        "function main() { const n = new Response().bytes(); }\n",
    );
    let (l, diags, _) = load(&both, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    let want = [
        "main",
        "std/prelude/global/web",
        "std/web",
        "std/prelude/global/web_bytes",
        "std/web_bytes",
    ];
    assert_eq!(paths(&l), want);
    // The module without a word, or a word without the module: not loaded.
    let module_only = t.write("p/module.vlt", "function main() { new Response(); }\n");
    let (l, _, _) = load(&module_only, opts());
    assert_eq!(paths(&l), ["main", "std/prelude/global/web", "std/web"]);
    let word_only = t.write("p/word.vlt", "function main() { const bytes = 1; }\n");
    let (l, _, _) = load(&word_only, opts());
    assert_eq!(paths(&l), ["main"]);
}

/// The modules of the repository's std a program loads whose paths start with one of
/// `prefixes`.
fn std_modules(main: &str, prefixes: &[&str]) -> Vec<String> {
    let t = Tree::new();
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let root = t.write("p/main.vlt", main);
    let opts = LoadOptions {
        std_root: Some(std),
        ..Default::default()
    };
    let (l, diags, _) = load(&root, opts);
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    paths(&l)
        .into_iter()
        .filter(|p| prefixes.iter().any(|x| p.starts_with(x)))
        .map(str::to_string)
        .collect()
}

/// The JSON modules of the repository's std a program loads (`JSON` and `JsonValue` are
/// globals: std/prelude/global/json.vlt).
fn json_modules(main: &str) -> Vec<String> {
    std_modules(main, &["std/encoding/json", "std/prelude/json"])
}

#[test]
fn a_program_that_names_no_json_loads_no_json_modules() {
    let hello = "console.log(\"hello, world\");\n";
    assert_eq!(json_modules(hello), Vec::<String>::new());
    let parse = "const n = JSON.parse<{ x: i64 }>(\"{\\\"x\\\":1}\");\nconsole.log(n.x);\n";
    let loaded = json_modules(parse);
    assert!(
        loaded.contains(&"std/prelude/json/json".to_string()),
        "{loaded:?}"
    );
    assert!(
        loaded.contains(&"std/encoding/json/reader".to_string()),
        "{loaded:?}"
    );
    // A type annotation names the global too; so does the std module that uses `JSON`.
    let typed = "function f(v: JsonValue): string { return v.stringify(); }\n";
    assert!(json_modules(typed).contains(&"std/prelude/json/json".to_string()));
    let csv =
        "import { parseCsv } from \"velt/encoding/csv\";\nconsole.log(parseCsv(\"a\").length);\n";
    assert!(json_modules(csv).contains(&"std/prelude/json/json".to_string()));
}

/// Std's number printing (velt/lang/number) is loaded for a program that may convert a number
/// to text (std/prelude/global/number_text.vlt): a template with an expression, `String(…)`,
/// `.toString(…)`, `+` with a string. Hello world, and a program that only logs numbers (the
/// runtime prints those), parse none of it.
#[test]
fn a_program_that_prints_no_number_loads_no_number_printing() {
    let loads = |main: &str, module: &str| {
        std_modules(main, &["std/lang/hooks/number_", "std/lang/number/"])
            .contains(&module.to_string())
    };
    let (integers, floats) = ("std/lang/number/integer", "std/lang/number/format");
    for main in [
        "console.log(\"hello, world\");\n",
        "const n = 7;\nconsole.log(n, n + 1);\n",
    ] {
        assert!(!loads(main, integers) && !loads(main, floats), "{main}");
    }
    // Integers, and numbers nothing marks as floats (std's float printer stays out: such a
    // float would take the runtime's printer).
    for main in [
        "const n = 7;\nconsole.log(`n=${n}`);\n",
        "const n = 7;\nconsole.log(String(n));\n",
        "const n = 7;\nconsole.log(n.toString());\n",
        "const n = 7;\nconsole.log(\"n=\" + n);\n",
        "let s = \"\";\ns += `${1}`;\nconsole.log(s);\n",
        "class R {\n  score: f64 = 0.5;\n}\nfor (let i = 0; i < 3; i++) {\n  console.log(`${i}`, new R().score);\n}\n",
        "function f(x: number): string {\n  return `${x}`;\n}\nconsole.log(f(1.5));\n",
    ] {
        assert!(loads(main, integers) && !loads(main, floats), "{main}");
    }
    // An operand that may be a float (docs/internals/design/lazy-std.md "Float operands").
    for main in [
        "console.log(`${1.5}`);\n",
        "const n = 7;\nconsole.log(`${n / 2}`);\n",
        "const n = 7;\nconsole.log(`${n ** 0.5}`);\n",
        "console.log(String(Math.sqrt(2)));\n",
        "const n = 7;\nconsole.log((n as f64).toString());\n",
        "const x: f64 = 1;\nconsole.log(`x=${x}`);\n",
        "const y = 3 / 4;\nconsole.log(\"y=\" + y);\n",
        "function f(x: f64): string {\n  return `${x}`;\n}\nconsole.log(f(1));\n",
        "const xs = [1, 2.5];\nfor (const x of xs) {\n  console.log(`${x}`);\n}\n",
        "const h: f16 = 1;\nconsole.log(`h=${h}`);\n",
    ] {
        assert!(loads(main, floats), "{main}");
    }
}

/// `expect`'s pattern matchers take a `Pattern` (std/text/regex/pattern), so a test file compiles the
/// regex engine only when it has a regex (#1889).
#[test]
fn a_test_file_without_a_regex_loads_no_regex_engine() {
    let plain = "import { expect, it } from \"velt/test\";\n\
                 it(\"m\", () => expect(\"hello\").toMatch(\"ll\"));\n";
    assert_eq!(
        std_modules(plain, &["std/text/regex/"]),
        ["std/text/regex/pattern"]
    );
    let literal = "import { expect, it } from \"velt/test\";\n\
                   it(\"m\", () => expect(\"hello\").toMatch(/^h/));\n";
    let loaded = std_modules(literal, &["std/text/regex/"]);
    assert!(
        loaded.contains(&"std/text/regex/regexp".to_string()),
        "{loaded:?}"
    );
}

/// An area's `index.vlt` re-exports its files by name; a program that imports one name loads
/// that file, not the area's other files (docs/internals/design/std-layout.md §2), but a file
/// with `extend` blocks loads whenever its area does (velt/simd's methods).
#[test]
fn an_area_import_loads_only_the_files_of_the_names_it_imports() {
    let deque =
        "import { Deque } from \"velt/collections\";\nconsole.log(new Deque<i64>().length);\n";
    let loaded = std_modules(deque, &["std/collections/"]);
    assert_eq!(loaded, ["std/collections/deque"]);
    let both = "import { Deque, BTreeMap } from \"velt/collections\";\n\
                console.log(new Deque<i64>().length, new BTreeMap<i64, i64>().size);\n";
    let loaded = std_modules(both, &["std/collections/"]);
    assert!(
        loaded.contains(&"std/collections/btree_map".to_string()),
        "{loaded:?}"
    );
    assert!(
        !loaded.contains(&"std/collections/concurrent_map".to_string()),
        "{loaded:?}"
    );
    let simd = "import { u8x16 } from \"velt/simd\";\nconsole.log(u8x16.splat(1).lane(0));\n";
    let loaded = std_modules(simd, &["std/simd/"]);
    assert!(loaded.contains(&"std/simd/words".to_string()), "{loaded:?}");
}
