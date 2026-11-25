//! A check by units (`VELT_UNITS=2`, velt_sema `units`) never writes std's frozen unit for what a
//! program raises of std's definitions: pass modes of a group or an interface slot, and throws.
//! Those are kept beside std's definitions (`units::raised`), and `@modifies` gives std's
//! generated functions their final modes, so the program raises none of them (#3543).

mod no_window;
mod test_dir;

/// `velt check` of `src`, by units as `units` says (`None`: a whole-program check), with `env`
/// set: whether it passed, and its stderr with `/` for path separators.
fn check(src: &str, units: Option<&str>, env: &[(&str, &str)]) -> (bool, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_UNITS_RAISED")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    match units {
        Some(v) => cmd.env("VELT_UNITS", v),
        None => cmd.env_remove("VELT_UNITS"),
    };
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr).replace('\\', "/");
    (out.status.success(), said)
}

/// A `toJSON` that changes its object: the JSON writers' instances raise their `value`.
const TO_JSON: &str = "class Tracked {\n  calls = 0;\n  toJSON(): { calls: number } {\n    this.calls++;\n    return { calls: this.calls };\n  }\n}\n\nfunction main() {\n  const t = new Tracked();\n  console.log(JSON.stringify(t));\n}\n";

/// An override of a std method that changes `this` (`Error.toString`, which has a slot in std's
/// unit): the vtable group raises std's method.
const OVERRIDE: &str = "class CountingError extends Error {\n  shown = 0;\n  override toString(): string {\n    this.shown++;\n    return \"CountingError \" + this.shown;\n  }\n}\n\nfunction main() {\n  const e: Error = new CountingError(\"boom\");\n  console.log(e.toString());\n}\n";

/// A program class implementing a std interface method that changes `this`: the slot's modes.
const SLOT: &str = "class Score implements Comparable<Score> {\n  compared = 0;\n  constructor(public value: i64) {}\n  compareTo(other: Score): i64 {\n    this.compared++;\n    return this.value - other.value;\n  }\n}\n\nfunction main() {\n  const c: Comparable<Score> = new Score(2);\n  console.log(c.compareTo(new Score(1)));\n}\n";

/// A program member joining a std throw group: an `Error` subclass whose `toString` throws raises
/// the throws of std's `Error.toString` (and the check refuses it: glue can't send the error).
const THROWS: &str = "class ThrowingError extends Error {\n  toString(): string {\n    throw new Error(\"toString threw\");\n  }\n}\n\nfunction main() {\n  const t = new ThrowingError(\"t\");\n  try {\n    console.log(String(t));\n  } catch (err) {\n    console.log(\"caught\", (err as Error).message);\n  }\n}\n";

/// The sites a program's raises wrote before `units::raised`.
const SITES: [&str; 3] = [
    "ownership/mutation.rs",
    "ownership/finish.rs",
    "throws/infer.rs",
];

#[test]
fn a_program_s_raises_leave_std_s_frozen_unit_unwritten() {
    for (what, src, raised, valid) in [
        (
            "toJSON",
            TO_JSON,
            "modes std/encoding/json/generated::writeJson",
            true,
        ),
        (
            "override",
            OVERRIDE,
            "modes std/prelude/errors::Error.toString",
            true,
        ),
        ("slot", SLOT, "slot compareTo", true),
        (
            "throws",
            THROWS,
            "throws std/prelude/errors::Error.toString",
            false,
        ),
    ] {
        let env = [("VELT_UNITS_FREEZE", "1"), ("VELT_UNITS_RAISED", "1")];
        let (ok, said) = check(src, Some("2"), &env);
        assert_eq!(ok, valid, "{what}:\n{said}");
        // The program does raise std's definition, beside it.
        let line = format!("velt-units-raised: {raised}");
        assert!(
            said.lines().any(|l| l == line),
            "{what}: no `{line}`:\n{said}"
        );
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
    let (ok, said) = check(src, Some("2"), &[("VELT_UNITS_RAISED", "1")]);
    assert!(ok, "{said}");
    for f in ["writeJson", "writeJsonTop", "readJson"] {
        let name = format!("velt-units-raised: modes std/encoding/json/generated::{f}");
        assert!(
            !said.lines().any(|l| l == name),
            "{f} was raised by the program:\n{said}"
        );
    }
}

/// An `Error` subclass whose `toString` throws: its errors join the `toString` group, raising the
/// throws of std's `Error.toString`, which glue cannot send anywhere. A check by units reports
/// every error the whole-program check does, at std's method too, reading what the program
/// raised. (It also reports std's `ArgError.toString`, in the same group: a check by units
/// checks all of std, the whole-program check only what the program reaches.)
#[test]
fn a_raised_std_to_string_that_can_throw_is_reported_by_units_too() {
    let errors = |said: &str| -> Vec<String> {
        (said.lines())
            .filter(|l| l.contains(": error: "))
            .map(|l| l[l.find("std/").or(l.find("main.velt")).unwrap_or(0)..].to_string())
            .collect()
    };
    let (whole_ok, whole) = check(THROWS, None, &[]);
    let (units_ok, units) = check(THROWS, Some("2"), &[]);
    assert!(!whole_ok && !units_ok, "{whole}\n{units}");
    let (whole, units) = (errors(&whole), errors(&units));
    assert!(
        (whole.iter())
            .any(|l| l.starts_with("std/prelude/errors.velt:")
                && l.contains("`toString` cannot throw")),
        "{whole:?}"
    );
    for l in &whole {
        assert!(units.contains(l), "by units, no `{l}`:\n{units:?}");
    }
}
