//! Globals loaded on demand ([`super::globals`]): by a name a user module mentions, and the
//! conditional ones by their std module and a word.

use super::tests::{load, messages, paths, Tree};
use super::*;

#[test]
fn globals_load_when_a_user_module_names_them() {
    let t = Tree::new();
    let std = t.path("std");
    t.write("std/prelude/index.velt", "import \"./core\";\n");
    t.write(
        "std/prelude/core.velt",
        "export function id(): i64 { return 0; }\n",
    );
    t.write(
        "std/prelude/global/web.velt",
        "export { fetch, Response } from \"velt/web\";\n",
    );
    t.write(
        "std/prelude/global/url.velt",
        "export { URL } from \"velt/net/url\";\n",
    );
    // A std module that mentions a global's name does not load it.
    t.write(
        "std/web.velt",
        "// URL\nexport function fetch() {}\nexport class Response {}\n",
    );
    t.write("std/net/url/index.velt", "export class URL {}\n");
    t.write("std/server/index.velt", "export class Response {}\n");
    let opts = || LoadOptions {
        std_root: Some(std.clone()),
        ..Default::default()
    };
    let plain = t.write("p/plain.velt", "// prefetch\nfunction main() {}\n");
    let (l, diags, _) = load(&plain, opts());
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert_eq!(paths(&l), ["std/prelude/core", "main"]);

    t.write(
        "p/lib.velt",
        "export async function get() { await fetch(); }\n",
    );
    let root = t.write(
        "p/main.velt",
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
        "p/own.velt",
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
        "std/prelude/global/web.velt",
        "export { Response } from \"velt/web\";\n",
    );
    t.write(
        "std/prelude/global/web_bytes.velt",
        "// load when std/web uses: bytes arrayBuffer\nimport \"velt/web_bytes\";\n",
    );
    t.write("std/web.velt", "export class Response {}\n");
    t.write(
        "std/web_bytes.velt",
        "import { Response } from \"velt/web\";\nextend Response {\n  bytes(): i64 { return 1; }\n}\n",
    );
    let opts = || LoadOptions {
        std_root: Some(std.clone()),
        ..Default::default()
    };
    // The module and a word: loaded, after the rest.
    let both = t.write(
        "p/both.velt",
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
    let module_only = t.write("p/module.velt", "function main() { new Response(); }\n");
    let (l, _, _) = load(&module_only, opts());
    assert_eq!(paths(&l), ["main", "std/prelude/global/web", "std/web"]);
    let word_only = t.write("p/word.velt", "function main() { const bytes = 1; }\n");
    let (l, _, _) = load(&word_only, opts());
    assert_eq!(paths(&l), ["main"]);
}

/// A tree with a conditional global (`web_bytes`: `std/web` and the word `bytes`).
fn bytes_tree() -> Tree {
    let t = Tree::new();
    t.write(
        "std/prelude/global/web.velt",
        "export { Response } from \"velt/web\";\n",
    );
    t.write(
        "std/prelude/global/web_bytes.velt",
        "// load when std/web uses: bytes\nimport \"velt/web_bytes\";\n",
    );
    t.write("std/web.velt", "export class Response {}\n");
    t.write("std/web_bytes.velt", "export const b = 1;\n");
    t
}

/// The module and the word in different extra roots, in either order: the global loads.
#[test]
fn a_conditional_global_loads_for_a_word_and_its_module_in_other_extra_roots() {
    let t = bytes_tree();
    let root = t.write("p/main.velt", "function main() {}\n");
    let word = t.write("p/word.velt", "export const bytes = 1;\n");
    let module = t.write("p/module.velt", "export const r = new Response();\n");
    for extra in [[word.clone(), module.clone()], [module, word]] {
        let mut sm = SourceMap::new();
        let mut diags = vec![];
        let opts = LoadOptions {
            std_root: Some(t.path("std")),
            extra_roots: &extra,
            ..Default::default()
        };
        let l = load_program(&mut sm, &root, opts, &mut diags).expect("loads");
        assert!(diags.is_empty(), "{:?}", messages(&diags));
        assert!(paths(&l).contains(&"std/web_bytes"), "{:?}", paths(&l));
    }
}

/// Each non-std module's words are split once per load, not once per extra root: `velt check`
/// of every std file adds hundreds of roots.
#[test]
fn many_extra_roots_split_each_module_s_words_once() {
    let t = bytes_tree();
    let root = t.write(
        "p/main.velt",
        "const r = new Response();\nfunction main() {}\n",
    );
    let extra: Vec<_> = (0..40)
        .map(|i| t.write(&format!("p/m{i}.velt"), "export const n = 1;\n"))
        .collect();
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let opts = LoadOptions {
        std_root: Some(t.path("std")),
        extra_roots: &extra,
        ..Default::default()
    };
    globals::SPLITS.with(|n| n.set(0));
    let l = load_program(&mut sm, &root, opts, &mut diags).expect("loads");
    assert!(diags.is_empty(), "{:?}", messages(&diags));
    assert!(!paths(&l).contains(&"std/web_bytes"));
    assert_eq!(globals::SPLITS.with(|n| n.get()), 41);
}

/// The modules of the repository's std a program loads whose paths start with one of
/// `prefixes`.
fn std_modules(main: &str, prefixes: &[&str]) -> Vec<String> {
    let t = Tree::new();
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let root = t.write("p/main.velt", main);
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
/// globals: std/prelude/global/json.velt).
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

/// The globals whose modules hold module state (`sessionStorage`, `navigator`: a module-level
/// `let` each) load for a name in code only: a comment that names one loads neither, which would
/// give the program module state (#3097), and code that uses one still loads it.
#[test]
fn a_comment_does_not_load_a_global_with_module_state() {
    let state = &["std/lang/storage/", "std/lang/navigator/"];
    let comments = "// the Storage and sessionStorage of a navigator (Navigator)\n\
                    /* sessionStorage */\nconsole.log(1);\n";
    assert_eq!(std_modules(comments, state), Vec::<String>::new());
    let storage = "sessionStorage.setItem(\"k\", \"v\");\nconsole.log(sessionStorage.length);\n";
    assert!(
        std_modules(storage, state).contains(&"std/lang/storage/storage".to_string()),
        "{:?}",
        std_modules(storage, state)
    );
    let nav = "console.log(navigator.hardwareConcurrency > 0);\n";
    assert!(
        std_modules(nav, state).contains(&"std/lang/navigator/navigator".to_string()),
        "{:?}",
        std_modules(nav, state)
    );
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

/// An area's `index.velt` re-exports its files by name; a program that imports one name loads
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
