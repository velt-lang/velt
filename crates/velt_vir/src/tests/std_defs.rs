//! A hook lowering asks for is there, or lowering stops with an internal error naming it
//! (lower/std_defs.rs, #2197): a hook whose module the program loads without it is never a
//! silent switch to other code (the conversion left a `TypeError`, a float printed another way).

use velt_sema::hir;

use super::{lower_ok, std_src};

/// `p` with the definition named `name` renamed, as if std had lost it while its module stays.
fn without(mut p: hir::Program, name: &str) -> hir::Program {
    let mut renamed = false;
    for d in &mut p.defs {
        if let hir::Def::Fn(f) = d {
            if f.name == name {
                f.name = format!("{name}Gone");
                renamed = true;
            }
        }
    }
    assert!(renamed, "no `{name}` in the program");
    p
}

const TO_RECORD: &str = "function main() {\n  const a: any = { x: 1 };\n  \
     const r = a as Record<string, number>;\n  console.log(r[\"x\"]);\n}\n";

#[test]
fn a_record_conversion_lowers_with_its_hook() {
    let v = lower_ok(&std_src::check_src(TO_RECORD));
    assert!(
        v.funcs.iter().any(|f| f.symbol.contains("anyToRecord")),
        "no call of anyToRecord:\n{v}"
    );
}

/// The feature's hook missing from its loaded module: an ICE, not the `TypeError` path.
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/any_record::anyToRecord` missing \
                           from its module, which the program loads"
)]
fn a_feature_hook_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(TO_RECORD);
    lower_ok(&without(p, velt_common::std_hook!(any_to_record)));
}

const PRINTS_A_FLOAT: &str =
    "function main() {\n  const x: number = 1.5;\n  console.log(String(x));\n}\n";

/// A fallback hook (`float_text`, whose place `number_text` takes where its module isn't loaded)
/// missing from its loaded module: an ICE, not the other printing.
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/number_float::floatText` missing \
                           from its module, which the program loads"
)]
fn a_fallback_hook_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(PRINTS_A_FLOAT);
    lower_ok(&without(p, velt_common::std_hook!(float_text)));
}

/// A hook lowering cannot do without, missing: an ICE naming it.
#[test]
#[should_panic(expected = "ICE: std hook `std/lang/hooks/number_text::intPush` missing")]
fn a_required_hook_missing_is_an_ice() {
    let src = "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n";
    let p = std_src::check_src(src);
    lower_ok(&without(p, velt_common::std_hook!(int_push)));
}
