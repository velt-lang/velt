//! `velt build` and `velt run` by units (`VELT_UNITS=2`, `crates/veltc/src/driver/all_std.rs`):
//! the program built is the one a check by units checked, and a source generator's host (JSON) is
//! still built as one program. The nightly's `golden-units` runs every golden this way; these
//! guard the build path on pull requests, and a run with std and the program checked in one
//! context (`VELT_UNITS=1`) too.

mod no_window;
mod test_dir;

/// A JSON reader (built by the generator's host) beside an `extend` block and a std subclass.
const PROGRAM: &str = r#"
extend string {
  shout(): string {
    return this.toUpperCase() + "!";
  }
}

class AppError extends Error {
  constructor(m: string) {
    super(m);
    this.name = "AppError";
  }
}

type Point = { x: number; y: number };

function main() {
  const p = JSON.parse<Point>(`{"x":1,"y":2}`);
  console.log("hi".shout(), p.x + p.y, new AppError("e").name);
}
"#;

const PRINTS: &str = "HI! 3 AppError\n";

/// `velt <args>` in `dir` by units, with the checker's pass timings on stderr.
fn velt_by_units(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    velt_by_units_in(dir, "2", args)
}

/// `velt <args>` in `dir` by units in mode `mode` (`VELT_UNITS`), with the checker's pass timings
/// on stderr.
fn velt_by_units_in(dir: &std::path::Path, mode: &str, args: &[&str]) -> std::process::Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_UNITS", mode)
        .env("VELT_SEMA_TIMINGS", "1")
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER")
        .output()
        .unwrap()
}

/// The program's check started its app stage from std's frozen unit: the check was by units.
fn checked_by_units(stderr: &str) -> bool {
    stderr.contains("units: copy std")
}

#[test]
fn a_build_by_units_builds_the_program_its_check_by_units_checked() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let built = velt_by_units(dir.path(), &["build", "main.velt", "-o", "main"]);
    let stderr = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{stderr}");
    assert!(
        checked_by_units(&stderr),
        "the build didn't check by units:\n{stderr}"
    );
    let exe = dir
        .path()
        .join(format!("main{}", std::env::consts::EXE_SUFFIX));
    let ran = no_window::command(exe).output().unwrap();
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&ran.stdout), PRINTS);
}

/// `velt run` of a JSON program by units: its generator's host is built as one program (by units
/// it would need the generated code it is building, and fail).
#[test]
fn a_run_by_units_runs_a_json_program() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let ran = velt_by_units(dir.path(), &["run", "main.velt"]);
    let stderr = String::from_utf8_lossy(&ran.stderr);
    assert!(ran.status.success(), "{stderr}");
    assert!(
        checked_by_units(&stderr),
        "the run didn't check by units:\n{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&ran.stdout), PRINTS);
}

/// A build by units for Windows lowers no std code a Windows program can't link: it loads every
/// std file but the import map's unselected variants, and the POSIX-only signal hook
/// (`#sched-signal-hook`, whose `@export`ed handler calls C `write`) is one there. Checked
/// through the VIR, so it runs on any OS.
#[test]
fn a_build_by_units_for_windows_lowers_no_posix_only_std_code() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let args = [
        "build",
        "main.velt",
        "--target",
        "x86_64-pc-windows-msvc",
        "--emit",
        "vir",
    ];
    let built = velt_by_units(dir.path(), &args);
    let stderr = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "{stderr}");
    assert!(checked_by_units(&stderr), "not checked by units:\n{stderr}");
    let vir = String::from_utf8_lossy(&built.stdout);
    for name in ["sched_signal", "sched__signal"] {
        assert!(
            !vir.contains(name),
            "the POSIX signal hook was lowered ({name})"
        );
    }
}

/// A `get` trap throwing a string, read through std's proxy hooks: std's unit inferred the
/// hooks (and std's callers of them, `callArrayMethod` among them) with the traps' sets empty,
/// and a round of the app's stage solves the traps' variables from the program's traps again
/// (an earlier round's solution, `never`, kept would leave the hooks throwing nothing).
const PROXY_GET: &str = r#"
const p: any = new Proxy({ a: 1 }, {
  get(t, k, r) {
    if (k === "bad") {
      throw "a string";
    }
    return Reflect.get(t, k, r);
  },
});
try {
  console.log(p.bad);
} catch (e) {
  console.log("caught", e);
}
"#;

/// A `set` and a `has` trap throwing classes: the program is checked again with the sets the
/// first check found, without variables, where std's trap variables stand for those sets.
const PROXY_TWO_TRAPS: &str = r#"
class E1 extends Error {}
class E2 extends Error {}

const s: any = new Proxy({ a: 1 } as any, {
  set(t, k, v, r) {
    if (k === "bad") {
      throw new E1("set");
    }
    return Reflect.set(t, k, v, r);
  },
});
const h: any = new Proxy({ a: 1 } as any, {
  has(t, k) {
    if (k === "bad") {
      throw new E2("has");
    }
    return Reflect.has(t, k);
  },
});

async function write(p: any, k: string): Promise<void> {
  p[k] = 1;
}

async function test(p: any, k: string): Promise<boolean> {
  return k in p;
}

async function run(): Promise<void> {
  await write(s, "bad").catch((e) => console.log("set", e instanceof E1, (e as Error).message));
  const r = await test(h, "bad").catch((e) => {
    console.log("has", e instanceof E2, (e as Error).message);
    return false;
  });
  console.log(r, await test(h, "a"));
}

run();
"#;

/// Programs whose proxy traps throw run by units as checked whole: std's proxy hooks and their
/// std callers throw what the program's traps throw (`throws::traps`).
#[test]
fn a_run_by_units_raises_std_s_proxy_hooks_to_what_the_traps_throw() {
    for (what, src, prints) in [
        ("get", PROXY_GET, "caught a string\n"),
        (
            "set and has",
            PROXY_TWO_TRAPS,
            "set true set\nhas true has\nfalse true\n",
        ),
    ] {
        let dir = test_dir::TestDir::new();
        std::fs::write(dir.path().join("main.velt"), src).unwrap();
        let ran = velt_by_units(dir.path(), &["run", "main.velt"]);
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "{what}: {stderr}");
        assert!(checked_by_units(&stderr), "{what}: not by units:\n{stderr}");
        assert_eq!(String::from_utf8_lossy(&ran.stdout), prints, "{what}");
    }
}

/// Overrides that throw where the base method doesn't: through the base type a call throws what
/// the overrides throw (their dispatch group's error type).
const OVERRIDES_THROW: &str = r#"
function boom(): number {
  throw new Error("boom");
}

class P {
  m(x: number = 1): number {
    return x;
  }
  get value(): number {
    return 1;
  }
  size(): number {
    return 1;
  }
}

class Q extends P {
  override m(x: number = boom()): number {
    return x;
  }
  override get value(): number {
    throw new Error("getter");
  }
  override size(): number {
    throw new Error("size");
  }
}

const ps: P[] = [new P(), new Q()];
for (const p of ps) {
  try {
    console.log(p.m());
  } catch (e) {
    console.log("caught", (e as Error).message);
  }
  try {
    console.log(p.value);
  } catch (e) {
    console.log("caught", (e as Error).message);
  }
  try {
    console.log(p.size());
  } catch (e) {
    console.log("caught", (e as Error).message);
  }
}
"#;

/// An abstract method returning `Promise<string>` rejects with what its `async` overrides throw:
/// a call through the base type settles with the overrides' result layout.
const OVERRIDES_REJECT: &str = r#"
class IoError extends Error {}

abstract class Source {
  abstract read(): Promise<number[], IoError>;
  abstract text(): Promise<string>;
}

class Disk extends Source {
  fail: bool;
  constructor(fail: bool) {
    super();
    this.fail = fail;
  }
  async read(): Promise<number[], IoError> {
    if (this.fail) {
      throw new IoError("read failed");
    }
    return [1, 2, 3];
  }
  async text(): Promise<string> {
    if (this.fail) {
      throw new IoError("text failed");
    }
    return "abc";
  }
}

async function show(s: Source, text: bool): Promise<void> {
  try {
    if (text) {
      console.log(await s.text());
    } else {
      console.log((await s.read()).length);
    }
  } catch (e) {
    const io: IoError = e;
    console.log(`caught ${io.message}`);
  }
}

async function main() {
  await show(new Disk(false), true);
  await show(new Disk(true), true);
  await show(new Disk(true), false);
}
"#;

/// With std and the program checked in one context (`VELT_UNITS=1`), the program's overrides join
/// its dispatch groups: the memos std's bodies built over std's definitions alone are dropped
/// before the program is collected (#3887). Built from them, a call through the base type
/// reported "the error type here is not known yet", and an `async` override's promise was read
/// with the base method's layout (a crash).
#[test]
fn a_run_by_units_in_one_context_gives_the_program_s_overrides_their_groups() {
    for (what, src, prints) in [
        (
            "throws",
            OVERRIDES_THROW,
            "1\n1\n1\ncaught boom\ncaught getter\ncaught size\n",
        ),
        (
            "rejects",
            OVERRIDES_REJECT,
            "abc\ncaught text failed\ncaught read failed\n",
        ),
    ] {
        let dir = test_dir::TestDir::new();
        std::fs::write(dir.path().join("main.velt"), src).unwrap();
        let ran = velt_by_units_in(dir.path(), "1", &["run", "main.velt"]);
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "{what}: {stderr}");
        assert!(
            stderr.contains("units: app"),
            "{what}: not by units:\n{stderr}"
        );
        assert_eq!(String::from_utf8_lossy(&ran.stdout), prints, "{what}");
    }
}
