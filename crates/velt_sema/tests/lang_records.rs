//! `Record<K, V>` (docs/internals/design/record.md): key types are checked where the type is
//! resolved and per instantiation of generics; a type-parameter key is treated conservatively;
//! `r.name` on an enum-keyed record passes the enum member with that value.

mod common;

use common::hir_walk::{calls, func};
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Callee, Def, ExprKind};

const RES: &str = "enum Res { Cpu = \"cpu\", Mem = \"mem\" }";

#[test]
fn enum_key_names_pass_the_member_with_that_value() {
    let p = ok_src(&format!(
        "{RES} function main() {{ const r = JSON.parse<Record<Res, i64>>(\"{{}}\");
           r.mem = 1; r.cpu += 1; console.log(r.mem); }}"
    ));
    let mut keyed = 0;
    for (c, args) in calls(func(&p, "main")) {
        let Callee::Def(d, _) = c else { continue };
        let Def::Fn(f) = p.def(*d) else { continue };
        if f.name.ends_with("Record.__at") || f.name.ends_with("Record.__set") {
            assert!(
                matches!(args[1].kind, ExprKind::Variant { .. }),
                "{}: {:?}",
                f.name,
                args[1].kind
            );
            keyed += 1;
        }
    }
    assert_eq!(keyed, 4);
}

#[test]
fn enum_key_names_must_be_values() {
    let e = err_src(&format!(
        "{RES} function main() {{ const r = JSON.parse<Record<Res, i64>>(\"{{}}\");
           console.log(r.Mem); }}"
    ));
    assert!(e.contains("`Res` has no key \"Mem\""), "{e}");
}

#[test]
fn written_key_types_are_checked_where_resolved() {
    for t in ["bool", "i64", "f64[]"] {
        let e = err_src(&format!(
            "function f(r: Record<{t}, i64>) {{}} function main() {{}}"
        ));
        assert!(
            e.contains(&format!("`{t}` cannot be a `Record` key")),
            "{e}"
        );
    }
    let e = err_src("function main() { JSON.parse<Record<i64, string>>(\"{}\"); }");
    assert!(e.contains("`i64` cannot be a `Record` key"), "{e}");
}

#[test]
fn generic_keys_are_checked_per_instantiation() {
    let src = "function dec<K>(s: string): Record<K, i64> { return JSON.parse<Record<K, i64>>(s); }
       function via<T>(): usize { return Object.keys(dec<T>(\"{}\")).length; }
       class Box<K> { r: Record<K, i64> = dec<K>(\"{}\"); }";
    ok_src(&format!(
        "{src} {RES} function main() {{ via<string>(); via<Res>(); new Box<\"a\" | \"b\">(); }}"
    ));
    let e = err_src(&format!("{src} function main() {{ via<f64>(); }}"));
    assert!(e.contains("`f64` cannot be a `Record` key"), "{e}");
    assert!(e.contains("required because `dec` uses it"), "{e}");
    let e = err_src(&format!("{src} function main() {{ new Box<bool>(); }}"));
    assert!(e.contains("`bool` cannot be a `Record` key"), "{e}");
}

#[test]
fn type_parameter_keys_cannot_start_empty_or_shrink() {
    for body in [
        "function mk<K>(): Record<K, i64> { return {}; }",
        "function mk<K>(): Record<K, i64> { return new Record<K, i64>(); }",
        "function drop<K>(r: Record<K, i64>, k: K) { delete r[k]; }",
    ] {
        let e = err_src(&format!("{body} function main() {{}}"));
        assert!(e.contains("the key type `K` is a type parameter"), "{e}");
    }
    ok_src(
        "function copy<K>(r: Record<K, i64>): Record<K, i64> { return { ...r }; }
         function get<K>(r: Record<K, i64>, k: K): i64 | null { return r[k]; }
         function main() {}",
    );
}

#[test]
fn record_internals_are_not_methods() {
    let e = err_src(
        "function main() { const r: Record<\"a\" | \"b\", i64> = { a: 1, b: 2 };
           r.__delete(\"a\"); }",
    );
    assert!(e.contains("`__delete` is internal to `Record`"), "{e}");
    assert!(e.contains("delete r[k]"), "{e}");
}
