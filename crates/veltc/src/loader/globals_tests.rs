//! Globals loaded on demand ([`super::globals`]): by a name a user module mentions, and the
//! conditional ones by their std module and a word.

use super::tests::{load, messages, paths, Tree};
use super::*;

#[test]
fn globals_load_when_a_user_module_names_them() {
    let t = Tree::new();
    let std = t.path("std");
    t.write(
        "std/prelude/core.vlt",
        "export function id(): i64 { return 0; }\n",
    );
    t.write(
        "std/prelude/global/web.vlt",
        "export { fetch, Response } from \"velt:web\";\n",
    );
    t.write(
        "std/prelude/global/url.vlt",
        "export { URL } from \"velt:url\";\n",
    );
    // A std module that mentions a global's name does not load it.
    t.write(
        "std/web.vlt",
        "// URL\nexport function fetch() {}\nexport class Response {}\n",
    );
    t.write("std/url.vlt", "export class URL {}\n");
    t.write("std/server.vlt", "export class Response {}\n");
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
        "import { Response } from \"velt:server\";\nclass URL {}\n\
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
        "export { Response } from \"velt:web\";\n",
    );
    t.write(
        "std/prelude/global/web_bytes.vlt",
        "// load when std/web uses: bytes arrayBuffer\nimport \"velt:web_bytes\";\n",
    );
    t.write("std/web.vlt", "export class Response {}\n");
    t.write(
        "std/web_bytes.vlt",
        "import { Response } from \"velt:web\";\nextend Response {\n  bytes(): i64 { return 1; }\n}\n",
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
