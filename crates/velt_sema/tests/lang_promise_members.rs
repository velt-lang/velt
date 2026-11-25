//! An arrow typed by a union of a `void` function type and one returning a promise
//! (`body::expr::closure_members`): a `return` of a promise in the arrow's own body takes the
//! promise member, a `return` in an arrow nested in it doesn't (#3710, #3716).

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, Program, TyKind};

/// Whether each closure made in function `f` returns an optional (the promise member's
/// `Promise<void> | undefined`), in definition order.
fn optional_results(p: &Program, f: &str) -> Vec<bool> {
    p.defs
        .iter()
        .filter_map(|d| match d {
            Def::Fn(c) if c.name.starts_with(&format!("{f}::{{closure#")) => {
                Some(matches!(p.types.kind(c.ret), TyKind::Option(..)))
            }
            _ => None,
        })
        .collect()
}

const PRELUDE: &str = "
    type Cb = ((x: number) => void) | ((x: number) => Promise<void> | undefined);
    function none(): Promise<void> | undefined { return undefined; }
    function take(cb: Cb) {}
";

#[test]
fn a_return_in_the_arrow_itself_takes_the_promise_member() {
    let p = ok_src(&format!(
        "{PRELUDE}
         function main() {{ take((x) => {{ return none(); }}); }}"
    ));
    assert_eq!(optional_results(&p, "main"), vec![true]);
}

/// A promise the probe counts as dropped rules out every member, so the trial falls back to the
/// first one. Here that is the `number` member, which the arrow doesn't fit, so counting the
/// nested arrow's `return` of a promise would make the program fail to compile.
#[test]
fn a_return_in_a_nested_arrow_leaves_the_void_member() {
    let p = ok_src(&format!(
        "{PRELUDE}
         type Cb3 = ((x: number) => number) | ((x: number) => void) | Cb;
         function take3(cb: Cb3) {{}}
         function main() {{
           take3((x) => {{
             [1].forEach((y) => {{ return none(); }});
           }});
         }}"
    ));
    let results = optional_results(&p, "main");
    assert!(!results.is_empty(), "no closures in main");
    assert!(
        results.iter().all(|o| !o),
        "a closure took the promise member: {results:?}"
    );
}
