//! The vtable slots a program's overrides add to std's classes, kept beside them by a check by
//! units (velt_sema `collect::program_slots`, #3684): a std subtree whose vtables have different
//! lengths gets the slot after the longest, padded, and a padded index pairs with nothing; an
//! override that gives a parameter another default gets std's class the default-filling slots.

mod no_window;
mod std_copy;
mod test_dir;

/// A std subtree whose subclass has a slot where its base has none: `PMid.c` (overridden by
/// `PLeaf`, which throws) is slot 0 of `PMid` and `PLeaf`, and `PBase` has no slot. A program
/// overriding `PBase.m` adds `m` after index 0, padding `PBase`'s index 0.
const PROBE: &str = r#"// velt/probe: a std subtree for the units slot tests
/** The base: no slot of its own. */
export class PBase {
  /** A method nothing in std overrides. */
  m(): number {
    return 1;
  }
}

/** A subclass whose `c` has a slot (`PLeaf` overrides it). */
export class PMid extends PBase {
  /** Overridden by `PLeaf`. */
  c(): number {
    return 2;
  }
}

/** Overrides `c` with one that throws. */
export class PLeaf extends PMid {
  /** Throws. */
  override c(): number {
    throw new RangeError("leaf");
  }
}
"#;

const PADDED: &str = r#"import { PBase, PMid } from "velt/probe";

class Three extends PBase {
  override m(): number {
    return 3;
  }
}

function main() {
  const b: PBase = new Three();
  console.log(b.m(), new PMid().m());
}
"#;

/// `velt check` of `src` by units (`VELT_UNITS=2`) with what the program raised beside std
/// (`VELT_UNITS_RAISED`), against `std` (the repository's when `None`): whether it passed, and
/// its stderr.
fn check_by_units(src: &str, std: Option<&std::path::Path>) -> (bool, String) {
    let dir = test_dir::TestDir::new();
    let app = dir.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(&app)
        .env("VELT_UNITS", "2")
        .env("VELT_UNITS_RAISED", "1")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    if let Some(std) = std {
        cmd.env("VELT_STD", std);
    }
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).replace('\\', "/");
    (out.status.success(), stderr)
}

/// The padded index of `PBase` pairs with nothing: were it paired with `PMid.c` by position,
/// `PBase.m` would join `c`'s throw group and take `PLeaf`'s `RangeError`.
#[test]
fn a_padded_slot_pairs_with_nothing() {
    let dir = test_dir::TestDir::new();
    let std = std_copy::with_probe(dir.path(), PROBE);
    let (ok, said) = check_by_units(PADDED, Some(&std));
    assert!(ok, "{said}");
    // Every class of the subtree gets the slot, after `PMid`'s and `PLeaf`'s slot 0.
    for class in ["PBase", "PMid", "PLeaf"] {
        let line = format!("velt-units-raised: vtable std/probe::{class} ");
        assert!(
            said.lines().any(|l| l.starts_with(&line)),
            "{class}:\n{said}"
        );
    }
    assert!(
        !said
            .lines()
            .any(|l| l.starts_with("velt-units-raised: throws std/probe::PBase.m")),
        "PBase.m joined PMid.c's throw group:\n{said}"
    );
}

/// An override of a std method that gives a parameter another default: std's class keeps the
/// default-filling slots beside it, so a call through it fills the override's default (the run
/// is golden `std_override_default_arg`).
#[test]
fn an_override_s_other_default_gets_std_s_class_its_fill_slots() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/lang/std_override_default_arg.velt"),
    )
    .unwrap();
    let (ok, said) = check_by_units(&src, None);
    assert!(ok, "{said}");
    let line = (said.lines())
        .find(|l| l.starts_with("velt-units-raised: vtable ") && l.contains("::Float32Array "))
        .unwrap_or_else(|| panic!("Float32Array kept no slot:\n{said}"));
    assert!(line.contains("<fill>join"), "no fill slot: {line}");
}

/// A std subtree whose methods std gave slots (`PLeaf` overrides them): `k` without fill slots
/// (`PLeaf.k` keeps the default), `w` with std's for `x` only (`PLeaf.w` changes only `x`).
const FILLS_PROBE: &str = r#"// velt/probe: a std subtree for the units fill-slot tests
/** The base. */
export class PBase {
  /** Overridden by `PLeaf` with the same default. */
  k(x: number = 1): number {
    return x;
  }

  /** Overridden by `PLeaf` with another default for `x` only. */
  w(x: number = 1, y: number = 2): string {
    return "PBase.w " + x + " " + y;
  }
}

/** A subclass of its own. */
export class PMid extends PBase {}

/** Overrides `k` keeping its default, and `w` with another `x`. */
export class PLeaf extends PMid {
  /** The same default. */
  override k(x: number = 1): number {
    return x + 100;
  }

  /** Another `x`. */
  override w(x: number = 5, y: number = 2): string {
    return "PLeaf.w " + x + " " + y;
  }
}
"#;

const FILLS: &str = r#"import { PBase, PMid } from "velt/probe";

class UMid extends PMid {
  override k(x: number = 7): number {
    return x;
  }

  override w(x: number = 1, y: number = 9): string {
    return "UMid.w " + x + " " + y;
  }
}

function viaBase(p: PBase): string {
  return p.k() + " " + p.w(3);
}

function main() {
  console.log(viaBase(new UMid()));
}
"#;

/// An override's other default for a std method std gave a slot: the missing default-filling
/// slots are added (`<fill>k` and its trailing one, where std has none; a wider `<fill>w` for
/// `y`, std's kept under `<fill><std>w`, and the trailing one for `w(x)`), so a call through a
/// `PBase` fills `UMid`'s defaults (`7`, and `y = 9` for `w(3)`), as Node does.
#[test]
fn an_override_s_other_default_for_a_slotted_std_method_gets_the_missing_fill_slots() {
    let dir = test_dir::TestDir::new();
    let std = std_copy::with_probe(dir.path(), FILLS_PROBE);
    let (ok, said) = check_by_units(FILLS, Some(&std));
    assert!(ok, "{said}");
    for class in ["PBase", "PMid", "PLeaf"] {
        let start = format!("velt-units-raised: vtable std/probe::{class} ");
        let line = (said.lines())
            .find(|l| l.starts_with(&start))
            .unwrap_or_else(|| panic!("{class} kept no slot:\n{said}"));
        let keys: Vec<&str> = line[start.len()..].split(',').collect();
        for key in [
            "<fill>k",
            "<fill>#0#k",
            "<fill>w",
            "<fill><std>w",
            "<fill>#1#w",
        ] {
            assert!(keys.contains(&key), "{class} has no {key}: {line}");
        }
    }
}
