//! Std's calls that leave out arguments of a method a program's override gives other defaults
//! (#3789, velt_sema `units::std_calls`): by units (`VELT_UNITS`) std's bodies are checked
//! before the program, so a left-out argument, or `undefined` written for it, took std's
//! default at the call. Once the program adds its default-filling slots the call takes the one
//! of that many arguments, and the override that runs applies its own defaults, as in Node.
//! A call no slot can redo (a value that may be `undefined` among the written arguments, a
//! spread of an unknown length, a call through std's own general fill slot) is reported, and
//! std has none. The std these run against is a copy with one more module
//! (`velt/probe`), since no std class has a method a program overrides with other defaults
//! that std calls leaving them out.

mod no_window;
mod std_copy;
mod test_dir;

/// The module added to the copy of std: `k` has a slot (`PLeaf` overrides it, keeping its
/// default), `w` has none.
const PROBE: &str = r#"// velt/probe: std calls leaving out arguments, for the units default tests
/** The base. */
export class PBase {
  /** Overridden by `PLeaf` with the same default. */
  k(x: number = 1): number {
    return x;
  }

  /** Nothing in std overrides it. */
  w(x: number = 1, y: number = 2): string {
    return "PBase.w " + x + " " + y;
  }

  /** A rest parameter after the default. */
  r(x: number = 1, ...rest: number[]): string {
    return "PBase.r " + x + " " + rest.length;
  }
}

/** A subclass of its own. */
export class PMid extends PBase {}

/** Overrides `k` keeping its default. */
export class PLeaf extends PMid {
  /** The same default. */
  override k(x: number = 1): number {
    return x + 100;
  }
}

/** Std's calls through a `PBase`, leaving arguments out or writing `undefined` for them. */
export function stdCalls(p: PBase): string {
  return p.k() + " " + p.w(3) + " " + p.w(4, undefined) + " " + p.k(undefined) + " " + p.w();
}

/** The same calls on a receiver whose type is a type parameter bounded by `PBase`. */
export function stdGeneric<T extends PBase>(p: T): string {
  return p.k() + " " + p.w(3);
}

/** A spread of a tuple writes its elements. */
export function stdTuple(p: PBase): string {
  const t: [number] = [3];
  return p.w(...t);
}

/** Calls no slot can redo: `x` may be `undefined` among the written arguments, as it is,
 * in parentheses or under `as`. */
export function stdGeneral(p: PBase, x?: number): string {
  return p.w(x, 5) + " " + p.w((x), 5) + " " + p.w(x as number | undefined, 5);
}

/** A method with a rest parameter: its slot of no arguments leaves the rest empty. */
export function stdRest(p: PBase): string {
  return p.r() + " " + p.r(undefined) + " " + p.r(2, 3);
}
"#;

const PROGRAM: &str = r#"import { PBase, PMid, stdCalls, stdGeneric, stdRest, stdTuple } from "velt/probe";

class UMid extends PMid {
  override k(x: number = 7): number {
    return x;
  }

  override w(x: number = 1, y: number = 9): string {
    return "UMid.w " + x + " " + y;
  }

  override r(x: number = 7, ...rest: number[]): string {
    return "UMid.r " + x + " " + rest.length;
  }
}

function main() {
  console.log(stdCalls(new UMid()));
  console.log(stdCalls(new PMid()));
  const b: PBase = new UMid();
  console.log(b.k(), b.w(3));
  console.log(stdGeneric(new UMid()), stdTuple(new UMid()));
  console.log(stdGeneric(new PMid()), stdTuple(new PMid()));
  console.log(stdRest(new UMid()));
  console.log(stdRest(new PMid()));
}
"#;

/// What Node prints: the override's own defaults where it runs, std's where it doesn't.
const PRINTS: &str = "7 UMid.w 3 9 UMid.w 4 9 7 UMid.w 1 9\n\
                      1 PBase.w 3 2 PBase.w 4 2 1 PBase.w 1 2\n\
                      7 UMid.w 3 9\n\
                      7 UMid.w 3 9 UMid.w 3 9\n\
                      1 PBase.w 3 2 PBase.w 3 2\n\
                      UMid.r 7 0 UMid.r 7 0 UMid.r 2 1\n\
                      PBase.r 1 0 PBase.r 1 0 PBase.r 2 1\n";

/// `velt <args>` of [`PROGRAM`] by units (`VELT_UNITS=units`) against a copy of std with
/// [`PROBE`], reporting what the program raised: whether it passed, its stdout and its stderr.
fn velt_by_units(args: &[&str], units: &str) -> (bool, String, String) {
    let dir = test_dir::TestDir::new();
    let std_copy = std_copy::with_probe(dir.path(), PROBE);
    let app = dir.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("main.velt"), PROGRAM).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg("main.velt")
        .current_dir(&app)
        .env("VELT_STD", &std_copy)
        .env("VELT_UNITS", units)
        .env("VELT_UNITS_RAISED", "1")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER")
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace('\\', "/");
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

/// The calls std's `stdCalls` makes take the program's fill slots, by both modes of units.
#[test]
fn std_s_left_out_arguments_take_the_program_s_fill_slots() {
    for units in ["1", "2"] {
        let (ok, _, said) = velt_by_units(&["check"], units);
        assert!(ok, "VELT_UNITS={units}: {said}");
        let fills = [
            ("stdCalls", "<fill>#0#k"),
            ("stdCalls", "<fill>#1#w"),
            ("stdCalls", "<fill>#0#w"),
            ("stdGeneric", "<fill>#0#k"),
            ("stdGeneric", "<fill>#1#w"),
            ("stdTuple", "<fill>#1#w"),
            ("stdRest", "<fill>#0#r"),
        ];
        for (f, key) in fills {
            let line = format!("velt-units-raised: fill std/probe::{f} -> {key}");
            assert!(
                said.contains(&line),
                "VELT_UNITS={units}: no {f} {key}:\n{said}"
            );
        }
    }
}

/// Run by units, debug and `--release`, the override that runs applies its own defaults, as
/// Node does.
#[test]
fn by_units_the_override_s_defaults_apply_in_std_s_calls() {
    for units in ["1", "2"] {
        for run in [&["run"][..], &["run", "--release"]] {
            let (ok, printed, said) = velt_by_units(run, units);
            assert!(ok, "VELT_UNITS={units} {run:?}: {said}");
            assert_eq!(printed, PRINTS, "VELT_UNITS={units} {run:?}");
        }
    }
}

/// `stdGeneral`'s three calls (`x` as it is, in parentheses, under `as`) are reported: no slot
/// the program adds can apply an override's default for `x` there.
#[test]
fn a_call_no_slot_can_redo_is_reported() {
    let (ok, _, said) = velt_by_units(&["check"], "2");
    assert!(ok, "{said}");
    let reported: Vec<&str> = (said.lines())
        .filter(|l| l.starts_with("velt-units-raised: unfillable call "))
        .collect();
    assert_eq!(reported.len(), 3, "{said}");
    assert!(reported.iter().all(|l| l.contains("std/probe@")), "{said}");
}

/// Std itself has no call no slot can redo (`a_call_no_slot_can_redo_is_reported`): such a call
/// of a class's method would pass std's default where a program's override has another, by
/// units (#3789).
#[test]
fn std_has_no_call_no_slot_can_redo() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), "console.log(1);\n").unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", "2")
        .env("VELT_UNITS_RAISED", "1")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let calls: Vec<&str> = (said.lines())
        .filter(|l| l.starts_with("velt-units-raised: unfillable call "))
        .collect();
    assert!(
        calls.is_empty(),
        "std calls a class's method in a way no slot a program adds can redo by units, so a \
         program's override can't apply its own default there (#3789): a value that may be \
         `undefined` among the written arguments, a spread of an unknown length that may fill \
         a parameter with a default, or a call through std's own general fill slot (listed even where it comes \
         out right: compare with Node). Write the arguments out, or call it as `m(a)` / \
         `m()`:\n{}",
        calls.join("\n")
    );
}
