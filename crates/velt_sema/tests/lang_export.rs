//! `@export("c_name")` (#2203 step 5): what may be exported to C, what it records in
//! `Program::c_exports`, and the errors for everything else.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{CTy, IntTy};

const HEAD: &str = "import * as ffi from \"velt/ffi\";\nimport { Ptr, export as cExport } from \"velt/ffi\";\ndeclare c type FILE;\n";

fn ok(rest: &str) -> velt_sema::hir::Program {
    ok_src(&format!("{HEAD}{rest}\nfunction main() {{}}\n"))
}

fn err(rest: &str) -> String {
    err_src(&format!("{HEAD}{rest}\nfunction main() {{}}\n"))
}

#[test]
fn an_exported_function_is_recorded_with_what_crosses() {
    let p = ok(r#"
@ffi.export("lib_add")
function add(a: i32, b: u64, c: f64, d: boolean): f32 { return 1; }
@cExport("lib_open")
function open(f: Ptr<FILE> | null, g: Ptr<u8>): Ptr<FILE> | null { return f; }
@cExport("lib_done")
function done(): void {}
"#);
    let got: Vec<(&str, &[CTy], CTy)> = (p.c_exports.iter())
        .map(|e| (e.name.as_str(), &e.params[..], e.ret))
        .collect();
    assert_eq!(
        got,
        [
            (
                "lib_add",
                &[
                    CTy::Int(IntTy::I32),
                    CTy::Int(IntTy::U64),
                    CTy::F64,
                    CTy::Bool
                ][..],
                CTy::F32
            ),
            (
                "lib_open",
                &[CTy::Ptr { nullable: true }, CTy::Ptr { nullable: false }][..],
                CTy::Ptr { nullable: true }
            ),
            ("lib_done", &[][..], CTy::Void),
        ]
    );
}

#[test]
fn a_program_without_exports_records_none() {
    assert!(ok("function f(a: i32): i32 { return a; }")
        .c_exports
        .is_empty());
}

#[test]
fn only_a_top_level_function_with_a_body_is_exported() {
    let e = err("class C {\n  @ffi.export(\"lib_m\")\n  m(): void {}\n}");
    assert!(e.contains("cannot be applied to a method"), "{e}");
    let e = err("@ffi.export(\"lib_d\")\ndeclare function d(): void;");
    assert!(e.contains("not to a `declare function`"), "{e}");
    let e = err("function outer(): void {\n  @ffi.export(\"lib_n\")\n  function inner(): void {}\n  inner();\n}");
    assert!(e.contains("not to one declared in a body"), "{e}");
}

#[test]
fn generic_async_and_generator_functions_are_refused() {
    let e = err("@ffi.export(\"lib_g\")\nfunction g<T>(x: i32): i32 { return x; }");
    assert!(e.contains("doesn't apply to a generic function"), "{e}");
    let e = err("@ffi.export(\"lib_a\")\nasync function a(): Promise<void> {}");
    assert!(e.contains("doesn't apply to an async function"), "{e}");
    let e = err("@ffi.export(\"lib_s\")\nfunction* s(): Generator<i32> { yield 1; }");
    assert!(e.contains("doesn't apply to a generator"), "{e}");
}

#[test]
fn only_what_a_declare_c_function_passes_crosses() {
    let e = err("@ffi.export(\"lib_s\")\nfunction s(x: string): void {}");
    assert!(
        e.contains(
            "parameter `x` of an `@export` function has type `string`, which can't cross from C"
        ),
        "{e}"
    );
    let e = err("@ffi.export(\"lib_r\")\nfunction r(): i32[] { return []; }");
    assert!(e.contains("returns `i32[]`, which can't cross to C"), "{e}");
    let e = err("@ffi.export(\"lib_o\")\nfunction o(x: i32 = 1): void {}");
    assert!(e.contains("can't have an optional parameter"), "{e}");
}

#[test]
fn the_name_is_a_c_identifier_nothing_else_claims() {
    for (name, why) in [
        ("1abc", "a C identifier"),
        ("a-b", "a C identifier"),
        ("velt_rt_alloc", "the Velt runtime's"),
        ("__x", "C reserves"),
        ("_Thing", "C reserves"),
        ("main", "the program's entry"),
        ("int", "a keyword of C"),
        ("free", "of the C library"),
    ] {
        let e = err(&format!("@ffi.export(\"{name}\")\nfunction f(): void {{}}"));
        assert!(
            e.contains("can't be the C name of an `@export` function"),
            "{name}: {e}"
        );
        assert!(e.contains(why), "{name}: {e}");
    }
}

#[test]
fn two_exports_of_one_name_and_a_declared_c_function_of_it_clash() {
    let e = err(
        "@ffi.export(\"lib_x\")\nfunction a(): void {}\n@cExport(\"lib_x\")\nfunction b(): void {}",
    );
    assert!(
        e.contains("two functions are exported to C as `lib_x`"),
        "{e}"
    );
    let e = err(
        "declare c(\"c\") function lib_y(): void;\n@ffi.export(\"lib_y\")\nfunction y(): void {}",
    );
    assert!(
        e.contains("`lib_y` is a C function the program declares"),
        "{e}"
    );
}

#[test]
fn an_exported_function_throws_nothing() {
    let e = err("@ffi.export(\"lib_t\")\nfunction t(x: i32): i32 { if (x < 0) { throw new Error(\"no\"); } return x; }");
    assert!(
        e.contains("the `@export` function `lib_t` may throw `Error`"),
        "{e}"
    );
    // Caught inside, it throws nothing.
    ok("@ffi.export(\"lib_c\")\nfunction c(x: i32): i32 { try { if (x < 0) { throw new Error(\"no\"); } } catch (e) { return -1; } return x; }");
}

#[test]
fn the_decorator_is_found_by_its_role_not_its_name() {
    // A program's own decorator named `export` is data, not the C export.
    let p = ok("declare decorator function export(name: string): FunctionDecorator;\n@export(\"lib_z\")\nfunction z(): void {}");
    assert!(p.c_exports.is_empty());
}

/// `exportedAddress` (std-only: the loader keeps programs out of velt/lang, which this harness
/// doesn't): its argument names an `@export` function.
#[test]
fn exported_address_takes_an_exported_function() {
    let intrinsics = "import { exportedAddress } from \"velt/lang/intrinsics\";\n";
    let p = ok(&format!(
        "{intrinsics}@ffi.export(\"lib_e\")\nfunction e(): void {{}}\nfunction g(): usize {{ return unsafe {{ exportedAddress(e) }}; }}"
    ));
    assert_eq!(p.c_exports.len(), 1);
    let e = err(&format!(
        "{intrinsics}function plain(): void {{}}\nfunction g(): usize {{ return unsafe {{ exportedAddress(plain) }}; }}"
    ));
    assert!(e.contains("takes the name of an `@export` function"), "{e}");
    let e = err(&format!(
        "{intrinsics}@ffi.export(\"lib_e\")\nfunction e(): void {{}}\nfunction g(): usize {{ return exportedAddress(e); }}"
    ));
    assert!(e.contains("unsafe"), "{e}");
}
