//! C pointers (`Ptr<T>`, `hir::TyKind::Ptr`, #3094): one machine word, and `Ptr<T> | null` the same
//! word with 0 for `null` (the null niche), in a parameter, a result, a class field and a C call,
//! with no hook called to convert one.

use super::c_wasi::{check_for, lower_checked_for};
use crate::vir::{self, Ty};

const LINUX: &str = "x86_64-unknown-linux-gnu";

/// A program passing pointers through a Velt function, a class field and a C function.
const SRC: &str = "import { Ptr } from \"velt/ffi\";\n\
    declare c type FILE;\n\
    declare c(\"c\") function fflush(f: Ptr<FILE> | null): i32;\n\
    class Handle { file: Ptr<FILE> | null = null; n: i32 = 0; }\n\
    function keep(p: Ptr<FILE> | null): Ptr<FILE> | null { return p; }\n\
    function keepSet(p: Ptr<FILE>, h: Handle): Ptr<FILE> { h.file = p; return p; }\n\
    function main() {\n  const h = new Handle();\n  h.file = keep(h.file);\n  \
    // SAFETY: never read through.\n  \
    console.log(keepSet(unsafe { Ptr.at<FILE>(16) }, h));\n  \
    // SAFETY: a test of lowering only; never run.\n  \
    console.log(unsafe { fflush(h.file) });\n}\n";

fn lowered() -> vir::Program {
    let (p, sm) = check_for(SRC, LINUX);
    lower_checked_for(&p, &sm, LINUX).unwrap_or_else(|e| panic!("lower:\n{e}"))
}

/// The function of `v` whose symbol contains `name`.
fn func<'a>(v: &'a vir::Program, name: &str) -> &'a vir::Function {
    (v.funcs.iter())
        .find(|f| f.symbol.contains(name))
        .unwrap_or_else(|| {
            panic!(
                "no `{name}` in {:?}",
                v.funcs.iter().map(|f| &f.symbol).collect::<Vec<_>>()
            )
        })
}

#[test]
fn a_nullable_pointer_is_one_word_in_a_function_s_signature() {
    let v = lowered();
    let keep = func(&v, "keep");
    assert_eq!(keep.params, vec![Ty::Ptr], "{}", keep.symbol);
    assert_eq!(keep.ret, Ty::Ptr, "{}", keep.symbol);
}

#[test]
fn a_nullable_pointer_field_is_one_word_of_the_object() {
    let v = lowered();
    let obj = (v.aggs.iter())
        .find(|a| a.name.contains("Handle"))
        .unwrap_or_else(|| panic!("no `Handle` layout"));
    assert_eq!(obj.fields[0].0, Ty::Ptr, "{obj:?}");
    assert_eq!(obj.fields[1].1 - obj.fields[0].1, 8, "{obj:?}");
}

#[test]
fn a_c_call_passes_the_word_and_calls_no_hook() {
    let v = lowered();
    let fflush = (v.externs.iter())
        .find(|x| x.symbol == "fflush")
        .unwrap_or_else(|| panic!("no `fflush` extern"));
    assert_eq!(
        (fflush.params.clone(), fflush.ret),
        (vec![Ty::Ptr], Ty::I32)
    );
    let hooks: Vec<&str> = (v.funcs.iter())
        .map(|f| f.symbol.as_str())
        .filter(|s| s.contains("ptrOf") || s.contains("ptrAddress") || s.contains("ptrOrNull"))
        .collect();
    assert!(
        hooks.is_empty(),
        "a pointer conversion hook is still called: {hooks:?}"
    );
}

/// A C pointer is no Velt object: nothing is known to be behind it, so a pointer parameter gets
/// no `nonnull` or `dereferenceable` (`abi.rs`, `PtrParam::NotPtr`), while a class parameter
/// of the same function does.
#[test]
fn a_pointer_parameter_has_no_pointer_attributes() {
    let v = lowered();
    let f = func(&v, "keepSet");
    assert_eq!(f.params, vec![Ty::Ptr, Ty::Ptr], "{}", f.symbol);
    assert!(f.param_attr(0).is_empty(), "{:?}", f.param_attrs);
    let object = f.param_attr(1);
    assert!(
        object.nonnull && object.dereferenceable > 0,
        "{:?}",
        f.param_attrs
    );
}
