//! Std's calls of a method a program's override gives a slot (#3684, velt_sema
//! `units::std_calls`): by units (`VELT_UNITS=2`) std's bodies are checked before the program, so
//! once the program adds the slot their direct calls dispatch where the override can run, and
//! stay direct where it can't: `super.m()`, a receiver just constructed, and a receiver whose
//! class no overriding program class extends. The std these run against is a copy with one more
//! module (`velt/probe`) holding those calls, since std has none of the first two today.

mod no_window;
mod std_copy;
mod test_dir;

/// The module added to the copy of std.
const PROBE: &str = r#"// velt/probe: std calls of `Map.get` for the units dispatch tests
/** A map whose methods call `get` in the ways a program's override must and must not reach. */
export class Probe extends Map<string, number> {
  /** `Map`'s own `get`, whatever overrides it. */
  first(): number | undefined {
    return super.get("a");
  }

  /** The `get` the object's class has. */
  plain(): number | undefined {
    return this.get("a");
  }
}

/** A map no program class extends. */
export class Other extends Map<string, number> {}

/** `get` on a receiver that is a `Map` and nothing else. */
export function freshGet(): number | undefined {
  return new Map<string, number>([["a", 1]]).get("a");
}

/** `get` on an `Other`. */
export function otherGet(o: Other): number | undefined {
  return o.get("a");
}

/** `get` on any `Probe`. */
export function probeGet(p: Probe): number | undefined {
  return p.get("a");
}
"#;

const PROGRAM: &str = r#"import { Probe, Other, freshGet, otherGet, probeGet } from "velt/probe";

class Loud extends Probe {
  override get(k: string): number | undefined {
    return 99;
  }
}

function main() {
  const p = new Loud();
  p.set("a", 1);
  const o = new Other();
  o.set("a", 2);
  console.log(p.first(), p.plain(), probeGet(p), freshGet(), otherGet(o));
}
"#;

/// What Node prints: `super.get` and the plain `Map`s run `Map`'s `get`, the others the override.
const PRINTS: &str = "1 99 99 1 2\n";

/// `velt run` of [`PROGRAM`] by units against a copy of std with [`PROBE`]: its stdout, and the
/// std functions whose calls dispatch (`velt-units-raised: dispatch` lines).
fn run_by_units() -> (String, Vec<String>) {
    let dir = test_dir::TestDir::new();
    let std_copy = std_copy::with_probe(dir.path(), PROBE);
    let app = dir.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("main.velt"), PROGRAM).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "main.velt"])
        .current_dir(&app)
        .env("VELT_STD", &std_copy)
        .env("VELT_UNITS", "2")
        .env("VELT_UNITS_RAISED", "1")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_DISK")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    let dispatched = (stderr.lines())
        .filter_map(|l| l.strip_prefix("velt-units-raised: dispatch std/probe::"))
        .map(str::to_string)
        .collect();
    (String::from_utf8_lossy(&out.stdout).to_string(), dispatched)
}

#[test]
fn std_s_calls_reach_a_program_override_only_where_it_can_run() {
    let (printed, dispatched) = run_by_units();
    assert_eq!(printed, PRINTS);
    let dispatches = |f: &str| dispatched.iter().any(|l| l.starts_with(f));
    // The calls on a receiver the program's class may be.
    assert!(dispatches("Probe.plain ->"), "{dispatched:?}");
    assert!(dispatches("probeGet ->"), "{dispatched:?}");
    // `super.get`, a fresh `Map`, and an `Other` stay direct.
    for direct in ["Probe.first ->", "freshGet ->", "otherGet ->"] {
        assert!(!dispatches(direct), "{direct} dispatches: {dispatched:?}");
    }
}
