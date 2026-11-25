//! `velt build` and `velt run` by units (`VELT_UNITS=2`, `crates/veltc/src/driver/all_std.rs`):
//! the program built is the one a check by units checked, and a source generator's host (JSON) is
//! still built as one program. The nightly's `golden-units` runs every golden this way; these
//! guard the build path on pull requests.

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
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_UNITS", "2")
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
