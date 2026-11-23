//! The methods of a live class that are live (#3580, `body/lazy_std/live_methods.rs`): a
//! method with a plain name that no glue calls is checked only where live code reaches it, by a
//! call, a table, an implementation or an `any`'s members; the others are checked with the
//! class. Each test names a std class's method, whose body is checked on demand.

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, Program};

const DATE: &str = "std/prelude/date::Date";

/// Is std function `name` checked in `p` (a body, not a stub in `unchecked`)?
fn checked(p: &Program, name: &str) -> bool {
    let found =
        (p.defs.iter().enumerate()).find(|(_, d)| matches!(d, Def::Fn(f) if f.name == name));
    let Some((i, _)) = found else {
        panic!("no function `{name}`");
    };
    !p.unchecked.iter().any(|u| u.0 as usize == i)
}

/// Is std function `name` in `p`'s live set (what lowering may build; a check may also check a
/// body it asks the result of, which isn't live)?
fn live(p: &Program, name: &str) -> bool {
    let set = p
        .live
        .as_ref()
        .expect("the live set (a compiler with assertions)");
    let found =
        (p.defs.iter().enumerate()).find(|(_, d)| matches!(d, Def::Fn(f) if f.name == name));
    let Some((i, _)) = found else {
        panic!("no function `{name}`");
    };
    set.contains(&velt_sema::hir::DefId(i as u32))
}

fn date(method: &str) -> String {
    format!("{DATE}.{method}")
}

fn program(top: &str, body: &str) -> Program {
    ok_src(&format!("{top}\nfunction main() {{\n{body}\n}}\n"))
}

#[test]
fn a_method_no_live_code_reaches_is_not_checked() {
    let p = program("", "  console.log(new Date(0).toISOString());");
    assert!(checked(&p, &date("toISOString")));
    // (`toString`, live with the class, calls `toDateString` and `toTimeString`.)
    for m in [
        "toUTCString",
        "toLocaleTimeString",
        "toLocaleString",
        "setMonth",
        "now",
    ] {
        assert!(!checked(&p, &date(m)), "{m} is checked");
    }
}

#[test]
fn the_constructor_and_the_hooks_glue_calls_are_checked_with_the_class() {
    // Nothing names them: `new` runs the constructor, and printing, `String()` and
    // `JSON.stringify` call `[INSPECT]()`, `toString()` and `toJSON()` from glue.
    let p = program("", "  const d = new Date(0);\n  console.log(d.getTime());");
    for m in [
        "constructor",
        "toJSON",
        "valueOf",
        "compareTo",
        "\u{FFFF}Symbol(nodejs.util.inspect.custom)",
    ] {
        assert!(checked(&p, &date(m)), "{m} is not checked");
    }
}

/// #3570's case: `toString()` (which reads the zone's name) is the text glue's, and a program
/// that turns no date into text has none of it.
#[test]
fn to_string_is_checked_only_where_a_value_of_the_class_becomes_text() {
    let p = program("", "  console.log(new Date(0).toISOString());");
    for m in ["toString", "toTimeString", "toDateString"] {
        assert!(!checked(&p, &date(m)), "{m} is checked");
    }
    for body in [
        "  console.log(String(new Date(0)));",
        "  const d = new Date(0);\n  console.log(`at ${d}`);",
        "  console.log(\"at \" + new Date(0));",
        "  const ds = [new Date(0)];\n  console.log(String(ds));",
        "  const d: Date | null = new Date(0);\n  console.log(`${d}`);",
    ] {
        let p = program("", body);
        assert!(live(&p, &date("toString")), "{body}");
        assert!(live(&p, &date("toTimeString")), "{body}");
    }
}

#[test]
fn a_subclass_value_as_text_makes_the_inherited_to_string_live() {
    let p = program(
        "class Stamp extends Date {}",
        "  console.log(String(new Stamp(0)));",
    );
    assert!(live(&p, &date("toString")));
    let p = program(
        "class Stamp extends Date {}",
        "  console.log(new Stamp(0).getTime());",
    );
    assert!(!live(&p, &date("toString")));
}

#[test]
fn a_type_parameter_as_text_makes_every_to_string_live() {
    // std's `join<T>` converts its elements to text: its instance's `toString()`, which the walk
    // sees as a type parameter's. (A program's own generic text conversion reads a member
    // dynamically, so every method is live there.)
    let p = program("", "  console.log([new Date(0)].join(\",\"));");
    assert!(live(&p, &date("toString")));
    assert!(!live(&p, &date("toLocaleString")), "every method is live");
}

#[test]
fn a_method_called_on_a_subclass_value_is_checked() {
    let p = program(
        "class Stamp extends Date {}",
        "  console.log(new Stamp(0).toUTCString());",
    );
    assert!(checked(&p, &date("toUTCString")));
    assert!(!checked(&p, &date("toLocaleTimeString")));
}

#[test]
fn a_method_a_subclass_overrides_is_checked_with_the_class() {
    // `Date`'s table has a slot for it, which lowering fills whichever call reaches it.
    let p = program(
        "class Stamp extends Date {\n  toUTCString(): string {\n    return \"stamp\";\n  }\n}",
        "  const d: Date = new Date(0);\n  console.log(d.getTime());",
    );
    assert!(checked(&p, &date("toUTCString")));
    assert!(!checked(&p, &date("toLocaleTimeString")));
}

#[test]
fn every_method_is_checked_once_a_member_of_an_any_is_read() {
    // The `any` tables' member slots call each class's methods by name.
    let p = program(
        "",
        "  const a: any = new Date(0);\n  console.log(a.getTime());",
    );
    for m in ["toUTCString", "toLocaleTimeString", "toTimeString"] {
        assert!(checked(&p, &date(m)), "{m} is not checked");
    }
}

#[test]
fn an_any_whose_members_nothing_reads_checks_no_more_methods() {
    let p = program("", "  const a: any = new Date(0);\n  console.log(a);");
    assert!(!checked(&p, &date("toLocaleTimeString")));
}

/// `Stamp` implements `Utc` with the method it inherits from `Date`.
const UTC: &str = "interface Utc {\n  toUTCString(): string;\n}\n\
                   class Stamp extends Date implements Utc {}";

#[test]
fn an_interface_value_checks_the_implementation() {
    let p = program(
        &format!("{UTC}\nfunction show(u: Utc): string {{\n  return u.toUTCString();\n}}"),
        "  console.log(show(new Stamp(0)));",
    );
    assert!(checked(&p, &date("toUTCString")));
    assert!(!checked(&p, &date("toLocaleTimeString")));
}

#[test]
fn a_bounded_type_parameter_checks_the_method_of_its_instance() {
    // Monomorphization builds `show<Stamp>`, whose call is the implementation's method.
    let p = program(
        &format!(
            "{UTC}\nfunction show<T extends Utc>(u: T): string {{\n  return u.toUTCString();\n}}"
        ),
        "  console.log(show(new Stamp(0)));",
    );
    assert!(checked(&p, &date("toUTCString")));
    assert!(!checked(&p, &date("toLocaleTimeString")));
}

#[test]
fn a_static_method_is_checked_where_it_is_called() {
    let p = program(
        "",
        "  console.log(new Date(0).getTime());\n  console.log(Date.UTC(2024, 0));",
    );
    assert!(checked(&p, &date("UTC")));
    assert!(!checked(&p, &date("now")));
}

#[test]
fn a_method_read_as_a_value_is_checked() {
    let p = program(
        "",
        "  const d = new Date(0);\n  const f = () => d.toUTCString();\n  console.log(f());",
    );
    assert!(checked(&p, &date("toUTCString")));
}

#[test]
fn a_getter_is_checked_with_its_class() {
    // Glue reads accessors by name (forwarders, reflection): kept with the class.
    let p = program(
        "",
        "  const m = new Map<string, i64>();\n  m.set(\"a\", 1);\n  console.log(m.has(\"a\"));",
    );
    assert!(checked(&p, "std/prelude/map::Map.size"));
}
