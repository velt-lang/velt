//! A check by units (`VELT_UNITS`, velt_sema `units`, #2943 M1): std collected and checked whole
//! as its own unit and frozen, then the program collected and checked against it. A program's
//! collection and bodies write nothing of std's (no `velt-units:` report), its `extend` blocks are
//! declared once, in its own unit, and a source generator's host (JSON) is checked as a whole
//! program as before.

mod no_window;
mod test_dir;

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

fn check(units: bool) -> std::process::Output {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    match units {
        true => cmd.env("VELT_UNITS", "1"),
        false => cmd.env_remove("VELT_UNITS"),
    };
    cmd.output().unwrap()
}

#[test]
fn a_check_by_units_writes_nothing_of_std_while_the_program_is_checked() {
    let out = check(true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("velt-units:"),
        "the program's check wrote std's definitions:\n{stderr}"
    );
    assert!(stderr.trim().is_empty(), "{stderr}");
}

#[test]
fn a_check_by_units_accepts_what_a_whole_program_check_does() {
    let whole = check(false);
    assert!(
        whole.status.success(),
        "{}",
        String::from_utf8_lossy(&whole.stderr)
    );
    let units = check(true);
    assert_eq!(whole.status.code(), units.status.code());
}

/// An override of a std method that has no slot would give std's class a slot: until D1 gives
/// every such method its slot in std's unit, the check counts it where the program crosses.
#[test]
fn an_override_of_a_std_method_without_a_slot_is_counted_as_a_crossing() {
    let dir = test_dir::TestDir::new();
    let src = r#"
class Loud extends Error {
  toString(): string {
    return "LOUD";
  }
}
function main() {
  console.log(String(new Loud("x")));
}
"#;
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", "1")
        .env_remove("VELT_UNITS_FREEZE")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // The report names the place by its source path, with the OS's separators.
    let places = stderr.replace('\\', "/");
    assert!(
        stderr.contains("velt-units:") && places.contains("collect/classes.rs"),
        "the crossing wasn't counted:\n{stderr}"
    );
}

/// The repository's goldens directory.
fn golden(path: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(path)
}

/// Checks the golden `path` (from its own directory, as goldens are) by units or not.
fn check_golden(path: &str, units: bool) -> std::process::Output {
    let file = golden(path);
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.arg("check")
        .arg(file.file_name().unwrap())
        .current_dir(file.parent().unwrap())
        .env_remove("VELT_UNITS_FREEZE");
    match units {
        true => cmd.env("VELT_UNITS", "1"),
        false => cmd.env_remove("VELT_UNITS"),
    };
    cmd.output().unwrap()
}

/// An app subclass redefining what a std `extend` block gives its base class is refused by units
/// as by a whole-program check: the app's classes meet std's `extend` blocks.
#[test]
fn a_check_by_units_refuses_redefining_a_std_extend_member() {
    let whole = check_golden("std/fetch_body_redefined.velt", false);
    let units = check_golden("std/fetch_body_redefined.velt", true);
    assert!(!whole.status.success());
    assert!(
        !units.status.success(),
        "{}",
        String::from_utf8_lossy(&units.stderr)
    );
    let shown = |o: &std::process::Output| {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .filter(|l| l.contains("error"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(&whole), shown(&units));
}

/// A golden whose object literals have the shape of one std made first: the app's check leaves
/// the shape's field spans to std (`Ctx::declare_anon_fields`), so nothing crosses.
#[test]
fn an_object_shape_std_made_first_is_not_written_by_the_app() {
    let out = check_golden("lang/_function_properties_generic_types.velt", true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("velt-units:"),
        "the program's check wrote std's definitions:\n{stderr}"
    );
}

/// A golden whose interfaces extend std's: the app's collection reads std's flattened
/// interfaces and never flattens them again (`collect::iface_extends`), so nothing crosses.
#[test]
fn an_app_interface_extending_std_ones_writes_nothing_of_std() {
    let out = check_golden("lang/comparable.velt", true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("velt-units:"),
        "the program's check wrote std's definitions:\n{stderr}"
    );
}

/// std is held whole in a check by units, but only the std modules the program links have state
/// when it runs (velt_sema `linked`): a `with` callback that calls a function value is refused
/// only when some state could be reached, and this program has none once its variable moves into
/// `main` (as a whole-program check finds).
#[test]
fn std_state_the_program_does_not_link_is_not_module_state() {
    let dir = test_dir::TestDir::new();
    let src = r#"
const m = shared(new Mutex<number>(0));
let c = 1;
const b = () => {
  c++;
};
const mk = () => {
  const add = () => c + 100;
  m.with((v) => {
    v += add();
  });
};
b();
mk();
console.log(m.with((v) => v));
"#;
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    for units in ["1", "2"] {
        let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["check", "main.velt"])
            .current_dir(dir.path())
            .env("VELT_UNITS", units)
            .env_remove("VELT_UNITS_FREEZE")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "VELT_UNITS={units}:\n{stderr}");
    }
}

/// The dispatch groups std's bodies were checked with know std's implementations only: once the
/// app is collected they are built again, with its classes (velt_sema `units`). Before, the
/// error types http-server's handlers saw through its own interfaces (`BodySink`) lacked what
/// its implementations throw, and `examples/todo_api.velt` failed with "not known yet".
#[test]
fn an_app_checked_by_units_sees_its_own_implementations_of_interfaces() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for units in ["1", "2"] {
        let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(["check", "todo_api.velt"])
            .current_dir(&examples)
            .env("VELT_UNITS", units)
            .env_remove("VELT_UNITS_FREEZE")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "VELT_UNITS={units}:\n{stderr}");
    }
}
