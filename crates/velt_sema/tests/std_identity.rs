//! The std identity oracle (`velt_sema::interface::std_defs`, #2943): a std definition's hash
//! is the same in two programs when its checked form is, whatever else each program loaded or
//! interned first, and the hashed form names definitions, types and implementations by what they
//! are, never by a number one check gave them.

mod common;

use common::programs::ok_src;
use velt_sema::interface::{std_def_texts, std_defs};

const HELLO: &str = "function main() { console.log(\"hi\"); }";

/// Loads another std module first, and makes types (a class, a union) before `main`, so other
/// definitions, types and files get other numbers than in `HELLO`.
const BUSIER: &str = "import { readFileSync } from \"velt/fs\";
class A { x = 1; }
type U = \"z\" | \"y\" | A;
function f(u: U) { console.log(u); }
function main() { f(\"y\"); console.log(\"hi\", readFileSync(\"/nope\").length); }";

#[test]
fn the_hashed_form_has_no_numbers_of_one_check() {
    let p = ok_src(BUSIER);
    let texts = std_def_texts(&p, "std/");
    assert!(texts.len() > 100, "{} std definitions", texts.len());
    for (name, text) in &texts {
        for raw in ["DefId(", "TyId(", "FileId(", "Span {"] {
            assert!(!text.contains(raw), "{name} has `{raw}`:\n{text}");
        }
        // An implementation's index is replaced by the implementation it names.
        let numbered = text
            .match_indices("impl_index: ")
            .any(|(at, m)| text[at + m.len()..].starts_with(|c: char| c.is_ascii_digit()));
        assert!(
            !numbered,
            "{name} names an implementation by its index:\n{text}"
        );
    }
    // Definitions are named: a call of another std function shows its name.
    assert!(
        texts.iter().any(|(_, t)| t.contains("@std/")),
        "no std definition names another"
    );
}

#[test]
fn a_std_definition_hashes_the_same_in_another_program() {
    let hello = std_defs(&ok_src(HELLO));
    let busier = std_defs(&ok_src(BUSIER));
    // Definitions both programs check and whose checked form depends on nothing either program
    // does: the same hashes, though their numbers differ.
    for name in [
        "std/lang/hooks/process::stdoutWritten",
        "std/lang/number/eisel_lemire::decide",
    ] {
        let a = hello.iter().find(|d| d.name == name).expect(name);
        let b = busier.iter().find(|d| d.name == name).expect(name);
        assert_eq!(a, b, "{name}");
    }
    // The same program checked twice gives the same summary.
    assert_eq!(hello, std_defs(&ok_src(HELLO)));
}

/// A module-level `let` of std (`sessionStorage`) is the same definition whether or not the
/// program's class objects come before it in the module state: its slot number is the
/// program's (`ModuleState::slots`), not the definition's (#2943).
#[test]
fn a_std_module_let_is_the_same_whatever_slots_come_first() {
    let plain = "function main() { console.log(sessionStorage.length); }";
    let with_objects = "function main() { const s: any = Set; const m: any = Map; console.log(typeof s, typeof m, sessionStorage.length); }";
    let a = std_defs(&ok_src(plain));
    let b = std_defs(&ok_src(with_objects));
    let name = "std/lang/storage/storage::sessionStorage";
    let find =
        |defs: &[velt_sema::interface::StdDef]| defs.iter().find(|d| d.name == name).cloned();
    assert!(find(&a).is_some(), "{name} is not checked");
    assert_eq!(find(&a), find(&b));
}

/// A std body sema writes for the program (`Program::glue_bodies`: `anyString` dispatching over
/// the program's classes) is left out: it is the same in no two programs, by design (#2943).
#[test]
fn the_programs_glue_is_not_compared() {
    let src = "class C { toString(): string { return \"c\"; } }
function main() { const x: any = new C(); console.log(String(x)); }";
    let defs = std_defs(&ok_src(src));
    let glue = "std/lang/hooks/any_text::anyString";
    assert!(defs.iter().all(|d| d.name != glue), "{glue} is compared");
    assert!(
        defs.iter()
            .any(|d| d.name.starts_with("std/lang/hooks/any_text::")),
        "no other any_text definition is checked"
    );
}
