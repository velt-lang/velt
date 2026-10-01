//! Every std module type-checks when imported (with the prelude), and the std APIs the M3/M4
//! goldens rely on keep their shapes (async functions, owned `this`, drop hooks).

mod common;

use common::hir_walk::{adt, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{PassMode, TyKind};

#[test]
fn every_std_module_type_checks() {
    for (module, name) in [
        ("velt:io", "IoError"),
        ("velt:fs", "readFile"),
        ("velt:net", "listen"),
        ("velt:net_bytes", "writeBytesTo"),
        ("velt:http", "serve"),
        ("velt:process", "argv"),
        ("velt:path", "join"),
        ("velt:json", "Value"),
        ("velt:math", "clamp"),
    ] {
        ok_src(&format!(
            "import {{ {name} }} from \"{module}\";\nfunction main() {{}}"
        ));
    }
}

#[test]
fn std_async_functions_own_their_params() {
    let p = ok_src("import { readFile } from \"velt:fs\";\nfunction main() {}");
    let f = func(&p, "std/fs::readFile");
    assert!(f.is_async);
    assert_eq!(
        f.ret,
        p.types.get(&TyKind::Str).unwrap(),
        "HIR `ret` of an async fn is `T`"
    );
    assert_eq!(f.params[0].mode, PassMode::Owned);
}

#[test]
fn std_handles_release_in_drop_hooks() {
    let p = ok_src("import { serve } from \"velt:http\";\nfunction main() {}");
    for class in [
        "std/http::Server",
        "std/http::Response",
        "std/http::FetchResponse",
    ] {
        assert!(
            adt(&p, class).dispose.is_some(),
            "{class} has no dispose hook"
        );
    }
    let text = func(&p, "std/http::FetchResponse.text");
    assert!(text.is_async);
    assert_eq!(
        text.params[0].mode,
        PassMode::Owned,
        "async `this` is owned"
    );
}

#[test]
fn json_values_clone_by_reference_but_containers_cannot() {
    ok_src("function main() { const v = JSON.parseValue(\"1\"); const w = v.clone(); console.log(w.isNumber()); }");
    let r = err_src(
        "struct Box { v: JsonValue; }
         function main() { const b = Box { v: JSON.parseValue(\"1\") }; const c = b.clone(); }",
    );
    assert!(r.contains("has no automatic `clone()`"), "{r}");
}
