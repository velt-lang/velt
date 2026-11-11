//! `hookCall("role", args…)` (std/lang/intrinsics/hook_call.velt, #2198): a function hook called by
//! its role, as std code calls it (only std may import the intrinsics).

mod common;

/// `src` checked as std code; the rendered diagnostics (empty when it checks).
fn std_diagnostics(src: &str) -> String {
    let mut l = common::programs::load_src(src);
    let root = l.root;
    l.modules[root].is_std = true;
    let (_, d) = l.check();
    l.render(&d)
}

const IMPORT: &str = "import { hookCall } from \"velt/lang/intrinsics\";";

fn std_err(body: &str) -> String {
    let d = std_diagnostics(&format!("{IMPORT}\n{body}"));
    assert!(d.contains("error"), "expected an error:\n{body}");
    d
}

#[test]
fn a_hook_call_is_checked_as_a_call_of_the_hook() {
    let d = std_diagnostics(&format!(
        "{IMPORT}\nfunction main() {{ const s: string = hookCall(\"float_text\", 1.5); console.log(s); }}"
    ));
    assert!(!d.contains("error"), "{d}");
    // The hook's own parameters and result type the call.
    let e = std_err("function main() { console.log(hookCall(\"float_text\", \"x\")); }");
    assert!(e.contains("mismatched types"), "{e}");
    let e = std_err(
        "function main() { const n: i64 = hookCall(\"float_text\", 1.5); console.log(n); }",
    );
    assert!(e.contains("mismatched types"), "{e}");
}

#[test]
fn a_hook_call_names_a_function_hook_by_a_literal() {
    let e = std_err("function main() { hookCall(); }");
    assert!(e.contains("`hookCall` needs the hook's role first"), "{e}");
    let e = std_err("function role(): string { return \"float_text\"; }\nfunction main() { console.log(hookCall(role(), 1.5)); }");
    assert!(
        e.contains("the role of `hookCall` must be a string literal"),
        "{e}"
    );
    let e = std_err("function main() { console.log(hookCall(\"no_such_hook\", 1)); }");
    assert!(e.contains("`no_such_hook` is no hook"), "{e}");
    let e = std_err("function main() { console.log(hookCall(\"decorator_inline\")); }");
    assert!(
        e.contains("the hook `decorator_inline` is not a function"),
        "{e}"
    );
}
