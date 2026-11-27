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

/// `p` with every definition of std module `module` named as the loader named it under a second
/// name (#3731: `std/../../…/std/…`, a path that reached std by another spelling).
fn under_second_name(p: hir::Program, module: &str) -> hir::Program {
    renamed_module(p, module, &format!("std/../../elsewhere/{module}"))
}

/// `p` with every definition of std module `module` named as module `to`'s.
fn renamed_module(mut p: hir::Program, module: &str, to: &str) -> hir::Program {
    let (from, to) = (format!("{module}::"), format!("{to}::"));
    let mut renamed = false;
    for d in &mut p.defs {
        let name = match d {
            hir::Def::Fn(f) => &mut f.name,
            hir::Def::Adt(a) => &mut a.name,
            _ => continue,
        };
        if let Some(item) = name.strip_prefix(&from) {
            *name = format!("{to}{item}");
            renamed = true;
        }
    }
    assert!(renamed, "no definition of `{module}` in the program");
    p
}

/// The feature's module loaded only under a second name: its hook is there, but not by the name
/// lowering asks for. An ICE naming both, not the feature switched off (#3745).
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/any_record::anyToRecord` missing, found as \
                `std/../../elsewhere/std/lang/hooks/any_record::anyToRecord`: its module is \
                loaded under a second name"
)]
fn a_feature_hook_under_a_second_module_name_is_an_ice() {
    let p = std_src::check_src(TO_RECORD);
    lower_ok(&under_second_name(p, "std/lang/hooks/any_record"));
}

/// A hook lowering cannot do without, its module loaded only under a second name: the ICE names
/// the name it was found by.
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/number_text::intPush` missing, found as \
                `std/../../elsewhere/std/lang/hooks/number_text::intPush`"
)]
fn a_required_hook_under_a_second_module_name_is_an_ice() {
    let src = "function main() {\n  const n: i64 = 7;\n  console.log(`n=${n}`);\n}\n";
    let p = std_src::check_src(src);
    lower_ok(&under_second_name(p, "std/lang/hooks/number_text"));
}

/// The program's own module under a `std` directory (`vendor/std/lang/hooks/any_record`, named
/// from the program's root) with the hook's item, and std's module not loaded: not a second name
/// of std's, so it lowers with the feature off, as any program without that module does.
#[test]
fn a_program_module_under_a_std_directory_is_not_a_second_name() {
    let p = std_src::check_src(TO_RECORD);
    let p = renamed_module(
        p,
        "std/lang/hooks/any_record",
        "vendor/std/lang/hooks/any_record",
    );
    lower_ok(&p);
}

const SPAWNS_A_GRAPH: &str = "class N {\n  next: N | null = null;\n}\n\
    async function take(n: N): Promise<number> {\n  return n.next === null ? 0 : 1;\n}\n\
    async function main() {\n  const n = new N();\n  const keep = n;\n  \
    console.log(await spawn(take(n)), keep.next === null);\n}\n";

/// The transfer map's hook, demanded by a `spawn` of a value, missing from its loaded module: an
/// ICE, not transfers left out. Both lower/transfer_map.rs's gate and the glue's calls of the
/// hook ask through `std_hook_if_loaded`, so either stops here.
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/transfer::xferBegin` missing from its module, \
                which the program loads"
)]
fn a_demanded_transfer_hook_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(SPAWNS_A_GRAPH);
    lower_ok(&without(p, velt_common::std_hook!(xfer_begin)));
/// Built-in methods of values held as `any`: a string's, an array's, a map's and a record's,
/// each called through `any`.
const ANY_METHODS: &str = "function main() {\n  const s: any = \" x \";\n  \
     console.log(s.trim());\n  const a: any = [1, 2];\n  console.log(a.indexOf(2));\n  \
     const m: any = new Map<string, number>([[\"k\", 1]]);\n  \
     m.forEach((v: any, k: any) => console.log(k, v));\n  \
     const rec: Record<string, number> = { x: 1 };\n  const r: any = rec;\n  \
     console.log(Reflect.ownKeys(r));\n}\n";

/// Each method lowering calls through `any` is a row of the hook table, found by its row: the
/// definitions it calls are the rows' (velt_common `std_hooks::any_methods`).
#[test]
fn the_methods_of_an_any_lower_through_their_rows() {
    let v = lower_ok(&std_src::check_src(ANY_METHODS));
    for h in [
        velt_common::std_hook_name!(any_str_trim),
        velt_common::std_hook_name!(any_arr_index_of),
        velt_common::std_hook_name!(any_map_for_each),
    ] {
        assert!(
            // The mangled name doubles an underscore (`anyStr__trim`).
            v.funcs
                .iter()
                .any(|f| f.symbol.contains(&h.replace('_', "__"))),
            "no call of {h}:\n{v}"
        );
    }
}

/// A method's row missing from its loaded module: an ICE naming it, where a scan by name prefix
/// left the method out (a `TypeError` naming #924 at run time).
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/any_methods::anyStr_trim` missing \
                           from its module, which the program loads"
)]
fn a_method_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(ANY_METHODS);
    lower_ok(&without(p, velt_common::std_hook!(any_str_trim)));
}

/// The same for a class's method (`Map`'s `forEach` through `any`).
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/hooks/any_array_methods::anyMap_forEach` missing \
                           from its module, which the program loads"
)]
fn a_class_method_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(ANY_METHODS);
    lower_ok(&without(p, velt_common::std_hook!(any_map_for_each)));
}

/// The same for a record's (`Reflect.ownKeys` of a record held as `any`).
#[test]
#[should_panic(
    expected = "ICE: std hook `std/lang/record/reflect::anyRec_ownKeys` missing \
                           from its module, which the program loads"
)]
fn a_record_method_its_loaded_module_lacks_is_an_ice() {
    let p = std_src::check_src(ANY_METHODS);
    lower_ok(&without(p, velt_common::std_hook!(any_rec_own_keys)));
}
