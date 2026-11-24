//! A check by units (`VELT_UNITS=2`, velt_sema `units`) never writes std's frozen unit for what a
//! program raises of std's definitions: pass modes of a group or an interface slot, and throws.
//! Those are kept beside std's definitions (`units::raised`), and `@modifies` gives std's
//! generated functions their final modes, so the program raises none of them (#3543).

mod no_window;
mod test_dir;

/// `velt check` of `src` by units, with `env` set; its stderr.
fn check_by_units(src: &str, env: &[(&str, &str)]) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", "2")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{said}");
    said
}

/// A `toJSON` that changes its object: the writer's instance raises its `value`.
const TO_JSON: &str = "class Tracked {\n  calls = 0;\n  toJSON(): { calls: number } {\n    this.calls++;\n    return { calls: this.calls };\n  }\n}\n\nfunction main() {\n  const t = new Tracked();\n  console.log(JSON.stringify(t));\n}\n";

/// An override of a std method that changes `this`: the vtable group raises std's method.
const OVERRIDE: &str = "class Counted extends Map<string, number> {\n  reads = 0;\n  override get(k: string): number | undefined {\n    this.reads++;\n    return super.get(k);\n  }\n}\n\nfunction main() {\n  const c = new Counted();\n  c.set(\"a\", 1);\n  const m: Map<string, number> = c;\n  console.log(m.get(\"a\"), c.reads);\n}\n";

/// A program class implementing a std interface method that changes `this`: the slot's modes.
const SLOT: &str = "class Counter implements Iterator<number> {\n  n = 0;\n  next(): IteratorResult<number> {\n    this.n++;\n    if (this.n > 2) {\n      return { done: true };\n    }\n    return { value: this.n, done: false };\n  }\n}\n\nfunction main() {\n  const it: Iterator<number> = new Counter();\n  console.log(it.next().value ?? 0);\n}\n";

/// A program member joining a std throw group: an implementation of a std interface method that
/// throws.
const THROWS: &str = "class Failing implements Iterator<number> {\n  next(): IteratorResult<number> {\n    throw new RangeError(\"no more\");\n  }\n}\n\nfunction main() {\n  const it: Iterator<number> = new Failing();\n  try {\n    console.log(it.next().done);\n  } catch (e) {\n    console.log(\"caught\");\n  }\n}\n";

/// The sites a program's raises wrote before `units::raised`.
const SITES: [&str; 3] = [
    "ownership/mutation.rs",
    "ownership/finish.rs",
    "throws/infer.rs",
];

#[test]
fn a_program_s_raises_leave_std_s_frozen_unit_unwritten() {
    for (what, src) in [
        ("toJSON", TO_JSON),
        ("override", OVERRIDE),
        ("slot", SLOT),
        ("throws", THROWS),
    ] {
        let said = check_by_units(src, &[("VELT_UNITS_FREEZE", "1")]);
        let written: Vec<&str> = (said.lines())
            .filter(|l| l.starts_with("velt-units:"))
            .filter(|l| SITES.iter().any(|s| l.contains(s)))
            .collect();
        assert!(
            written.is_empty(),
            "{what}: std's unit written at\n{}",
            written.join("\n")
        );
    }
}

/// `@modifies` on std's JSON writers and reader: a program that writes and reads JSON raises
/// none of their declarations (without the lists, every program's instances raise `w` and `r`).
#[test]
fn std_s_listed_generated_functions_are_not_raised_by_a_program() {
    let src = "interface P {\n  x: number;\n  name: string;\n}\n\nfunction main() {\n  const p: P = { x: 1, name: \"a\" };\n  const text = JSON.stringify(p);\n  const back = JSON.parse(text) as P;\n  console.log(text, back.x);\n}\n";
    let said = check_by_units(src, &[("VELT_UNITS_RAISED", "1")]);
    for f in ["writeJson", "writeJsonTop", "readJson"] {
        let name = format!("velt-units-raised: modes std/encoding/json/generated::{f}");
        assert!(
            !said.lines().any(|l| l == name),
            "{f} was raised by the program:\n{said}"
        );
    }
}

/// An `Error` subclass whose `toString` throws: its errors join the `toString` group, raising the
/// throws of std's `Error.toString`, which glue cannot send anywhere. Both the whole-program
/// check and a check by units report it at std's method, reading what the program raised.
#[test]
fn a_raised_std_to_string_that_can_throw_is_reported_by_units_too() {
    let src = "class ThrowingError extends Error {\n  toString(): string {\n    throw new Error(\"toString threw\");\n  }\n}\n\nfunction main() {\n  const t = new ThrowingError(\"t\");\n  try {\n    console.log(String(t));\n  } catch (err) {\n    console.log(\"caught\", (err as Error).message);\n  }\n}\n";
    let reports = |units: Option<&str>| {
        let dir = test_dir::TestDir::new();
        std::fs::write(dir.path().join("main.velt"), src).unwrap();
        let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(["check", "main.velt"])
            .current_dir(dir.path())
            .env_remove("VELT_UNITS_FREEZE")
            .env_remove("VELT_UNITS_DISK")
            .env_remove("VELT_STD_SUMMARY_EAGER");
        match units {
            Some(v) => cmd.env("VELT_UNITS", v),
            None => cmd.env_remove("VELT_UNITS"),
        };
        let out = cmd.output().unwrap();
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    for units in [None, Some("2")] {
        let said = reports(units);
        assert!(
            said.contains("std/prelude/errors.velt") && said.contains("toString cannot throw"),
            "VELT_UNITS={units:?}:\n{said}"
        );
    }
}
