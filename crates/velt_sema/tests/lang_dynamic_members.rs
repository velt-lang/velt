//! Members of an `any` value (body/expr/dynamic_member.rs): the source text a failed call names.

mod common;

use common::hir_walk::{exprs, func};
use common::programs::ok_src;
use velt_sema::hir::{ExprKind as E, Lit};

/// The string literals of `main`.
fn texts(src: &str) -> Vec<String> {
    let p = ok_src(src);
    exprs(func(&p, "main"))
        .into_iter()
        .filter_map(|e| match &e.kind {
            E::Lit(Lit::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_failed_call_names_a_number_key_as_js_prints_it() {
    // Node's `TypeError: o[…] is not a function` spells the key as `String(n)` (#3769): the
    // shortest digits that round-trip, the exponent form from 1e21 and below 1e-6.
    for (key, text) in [
        ("1658206780088562.25", "o[1658206780088562.2]"),
        ("1e21", "o[1e+21]"),
        ("1e-7", "o[1e-7]"),
        ("-0", "o[0]"),
        ("1.5", "o[1.5]"),
        ("123456789012345680000", "o[123456789012345680000]"),
        ("-2.5", "o[-2.5]"),
    ] {
        let lits = texts(&format!(
            "function main(): void {{ const o: any = {{}}; o[{key}](); }}"
        ));
        assert!(lits.iter().any(|s| s.contains(text)), "{key}: {lits:?}");
    }
}
