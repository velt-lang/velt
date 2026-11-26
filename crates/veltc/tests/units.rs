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
    check_in(if units { Some("1") } else { None }, PROGRAM)
}

/// `velt check` of `src`, by units as `VELT_UNITS` says (`None`: a whole-program check).
fn check_in(units: Option<&str>, src: &str) -> std::process::Output {
    check_files(units, &[("main.velt", src)])
}

/// `velt check` of `main.velt` among `files` (name, source), by units as `VELT_UNITS` says.
fn check_files(units: Option<&str>, files: &[(&str, &str)]) -> std::process::Output {
    let dir = test_dir::TestDir::new();
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).unwrap();
    }
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_STD_SUMMARY_EAGER");
    match units {
        Some(v) => cmd.env("VELT_UNITS", v),
        None => cmd.env_remove("VELT_UNITS"),
    };
    cmd.output().unwrap()
}

/// std checked as a check of its own and frozen, then the app's rounds from a copy of it
/// (`VELT_UNITS=2`): a program is accepted, and one with an error refused with the same
/// errors, as by a whole-program check.
#[test]
fn std_checked_as_its_own_unit_checks_the_app_as_a_whole_program_check_does() {
    let ok = check_in(Some("2"), PROGRAM);
    let stderr = String::from_utf8_lossy(&ok.stderr);
    assert!(ok.status.success(), "{stderr}");
    let bad = "function main() {\n  const n: number = \"text\";\n  console.log(n);\n}\n";
    let (whole, units) = (check_in(None, bad), check_in(Some("2"), bad));
    assert!(!whole.status.success());
    assert_eq!(whole.status.code(), units.status.code());
    let errors = |o: &std::process::Output| {
        (String::from_utf8_lossy(&o.stderr).lines())
            .filter(|l| l.contains("error"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(errors(&whole), errors(&units));
    // The app's round did start from std's copy.
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), PROGRAM).unwrap();
    let timed = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", "2")
        .env("VELT_SEMA_TIMINGS", "1")
        .output()
        .unwrap();
    let passes = String::from_utf8_lossy(&timed.stderr);
    assert!(passes.contains("units: copy std"), "{passes}");
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
    let stderr = crossings("1");
    assert!(
        stderr.contains("velt-units:") && stderr.contains("collect/classes.rs"),
        "the crossing wasn't counted:\n{stderr}"
    );
}

/// With std checked as a unit of its own (`VELT_UNITS=2`), the app's rounds start from a frozen
/// copy of it: the same write is reported. Building the program after the app's passes writes
/// every definition and is not counted.
#[test]
fn std_s_frozen_copy_reports_the_app_writing_std() {
    let stderr = crossings("2");
    assert!(
        stderr.contains("velt-units:") && stderr.contains("collect/classes.rs"),
        "the app's copy of std wasn't frozen:\n{stderr}"
    );
    assert!(
        !stderr.contains("finalize.rs"),
        "building the program was counted as the app writing std:\n{stderr}"
    );
}

/// `velt check`'s report of writes to std's definitions (`velt-units:` lines, places with `/`)
/// for a program overriding a std method that has no slot, checked by units as `VELT_UNITS`
/// says.
///
/// The method must stay slot-less in std: no std class may override `URLSearchParams.getAll`
/// (std/net/url/search_params.velt). An override in std gives the method its slot there, and the
/// app's override then crosses nothing (as `Error.toString` did once std's `ArgError` overrode
/// it).
fn crossings(units: &str) -> String {
    let dir = test_dir::TestDir::new();
    let src = r#"
class Loud extends URLSearchParams {
  getAll(name: string): string[] {
    return [name, "LOUD"];
  }
}
function main() {
  console.log(new Loud("a=1").getAll("a").join(","));
}
"#;
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", units)
        .env_remove("VELT_UNITS_FREEZE")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // The report names the place by its source path, with the OS's separators.
    stderr.replace('\\', "/")
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

/// `velt check` of the golden `path` from its own directory, with `VELT_UNITS=2` (std a unit of
/// its own) or not at all (`two` false: a whole-program check).
fn check_golden_two(path: &str, two: bool) -> std::process::Output {
    let file = golden(path);
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.arg("check")
        .arg(file.file_name().unwrap())
        .current_dir(file.parent().unwrap())
        .env_remove("VELT_UNITS_FREEZE")
        .env_remove("VELT_UNITS_DISK");
    match two {
        true => cmd.env("VELT_UNITS", "2"),
        false => cmd.env_remove("VELT_UNITS"),
    };
    cmd.output().unwrap()
}

/// std's unit ends clean (`units::finish_std`): the app's rounds start from no memo built over
/// std alone, so a package's recursive functions get no error a whole-program check does not
/// give, and its extended layouts are not sealed yet, so the app's object types share a layout
/// with their extensions; and the app's collection defers a type query that needs a body, as a
/// whole program's does. All check with `VELT_UNITS=2` as by a whole-program check.
#[test]
fn std_s_unit_ends_clean_and_the_app_is_collected_as_a_whole_program_is() {
    for path in [
        "lang/errors/http_server/http_handler_direct_null.velt",
        "lang/errors/typeof_generic_params_mismatch.velt",
        "lang/type_queries.velt",
        "lang/type_queries_keys.velt",
        "lang/module_augment_scopes.velt",
        "lang/errors/extended_layouts_keys.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        assert_eq!(whole.status.code(), two.status.code(), "{path}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
    }
}

/// std is held whole in a check by units, but only the std modules the program links have state
/// when it runs (velt_sema `linked`): a `with` callback that calls a function value is refused
/// only when some state could be reached, and this program has none of its own, as a
/// whole-program check finds.
#[test]
fn std_state_the_program_does_not_link_is_not_module_state() {
    let src = r#"
function main() {
  const m = shared(new Mutex<number>(0));
  const add = () => 100;
  const run = (f: () => number) => {
    m.with((v) => {
      v += f();
    });
  };
  run(add);
  console.log(m.with((v) => v));
}
"#;
    let whole = check_in(None, src);
    let stderr = String::from_utf8_lossy(&whole.stderr);
    assert!(whole.status.success(), "{stderr}");
    for units in ["1", "2"] {
        let out = check_in(Some(units), src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "VELT_UNITS={units}:\n{stderr}");
    }
    // The same program reading `navigator` links std's module that holds it, through a name in
    // its code and no import: that module's state is the program's, and the callback is refused.
    let src = src.replace(
        "m.with((v) => v))",
        "m.with((v) => v), navigator.hardwareConcurrency)",
    );
    let whole = check_in(None, &src);
    let errors = error_lines(&whole);
    assert!(
        errors.iter().any(|l| l.contains("may use module state")),
        "{errors:?}"
    );
    for units in ["1", "2"] {
        assert_eq!(
            errors,
            error_lines(&check_in(Some(units), &src)),
            "VELT_UNITS={units}"
        );
    }
}

/// With std checked as a unit of its own (`VELT_UNITS=2`), what std's bodies do with module
/// state comes from std's unit (`StateFacts`), not from reading std's bodies again: a `with`
/// callback that reaches the program's state through std's code (a generator's dispose hook,
/// a sort's comparison) is refused with the path through it, as by a whole-program check.
#[test]
fn std_s_unit_keeps_what_its_bodies_do_with_module_state() {
    for path in [
        "lang/errors/module_let_mutex_with.velt",
        "lang/errors/mutex_with_opaque.velt",
        "lang/errors/rwlock_read_probe_m3.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        assert!(!whole.status.success(), "{path}");
        assert_eq!(whole.status.code(), two.status.code(), "{path}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
    }
}

/// With std checked as a unit of its own, std's unit holds all of std, of which only what lazy std
/// would have checked is the program's (`lazy_std::units_live_set`): the async closures' analysis
/// reads only that code of std's (`lazy_std::walks`), so a closure changing a value it reached
/// through std's http server is reported with the field it changes, a closure sent on a channel
/// (std's `Channel.send`, whose body the analysis follows) is refused, and a program whose async
/// closures stay local is accepted, each as by a whole-program check.
#[test]
fn the_async_closures_analysis_reads_the_std_code_the_program_has() {
    for path in [
        "lang/errors/async_closure_crosses.velt",
        // A call through a function read from an `any` names the program's method, not std's.
        "lang/errors/http_server/http_handler_code_on_write.velt",
        "lang/errors/http_server/http_handler_store_any_read_fn.velt",
        "lang/errors/http_server/http_handler_store_any_read_method.velt",
        "lang/async_closure_generic_unknown_targ_others.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        assert_eq!(whole.status.code(), two.status.code(), "{path}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
    }
}

/// A whole-program check checks the program's bodies with std's modes as declared, and only its
/// passes after the bodies judge what a call of std changes. std's unit keeps the declared modes
/// for the app's bodies and the raised ones for its passes (`units::modes`). So a spawned closure
/// calling `Map.set` on a captured map, `Headers.get` (which fills a cache) on captured headers,
/// or `next` on a captured `Iterator` (an interface slot, the join over its implementations), is
/// not refused at the call, and the http handlers' changes through std's methods are
/// reported with the call that makes them. Each checks with `VELT_UNITS=2` as by a
/// whole-program check.
#[test]
fn the_app_s_bodies_see_std_s_modes_as_declared() {
    for src in [
        "async function main() {\n  const seen = new Map<string, number>();\n  await spawn((async () => {\n    seen.set(\"a\", 1);\n  })());\n}\n",
        "async function main() {\n  const h = new Headers({ \"x-a\": \"1\" });\n  await spawn((async () => {\n    console.log(h.get(\"x-a\") ?? \"\");\n  })());\n}\n",
        // Through an interface slot, whose modes are the join over its implementations.
        "function* numbers(): Generator<number> {\n  yield 1;\n}\n\nasync function main() {\n  const it: Iterator<number> = numbers();\n  await spawn((async () => {\n    console.log(it.next().value ?? 0);\n  })());\n}\n",
        // An interface of the program extending std's, which copies std's slots when the
        // program is collected (`collect/iface_extends.rs`).
        "interface Counted extends Iterator<number> {\n  count(): number;\n}\n\nfunction* nums(): Generator<number> {\n  yield 1;\n}\n\nasync function main() {\n  const it: Iterator<number> = nums();\n  const c: Counted = {\n    next: () => it.next(),\n    count: () => 1,\n  };\n  await spawn((async () => {\n    console.log(c.next().value ?? 0, c.count());\n  })());\n}\n",
    ] {
        let whole = check_in(None, src);
        let two = check_in(Some("2"), src);
        assert_eq!(whole.status.code(), two.status.code(), "{src}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{src}");
    }
    let path = "lang/errors/http_server/http_handler_changes_through_call.velt";
    let whole = check_golden_two(path, false);
    let two = check_golden_two(path, true);
    assert_eq!(whole.status.code(), two.status.code(), "{path}");
    assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
}

/// std's unit resolves the types of its `ProxyHandler` traps before any program stores into
/// them; a whole-program check resolves them from its seeds once the program's traps throw. So
/// std's unit keeps its trap sites (`throws::traps`): the app's rounds hold a trap closure to
/// what their seeds give its site, and a value whose type holds std's trap variables (a
/// rejection reason) is read as written. A trap that throws is accepted and the reason is the
/// trap's error; a trap that throws nothing stays quiet; the proxy-throws goldens check alike.
#[test]
fn a_proxy_trap_s_errors_reach_the_program_as_in_a_whole_program_check() {
    let throwing = "class E1 extends Error {}\n\nfunction make(): any {\n  return new Proxy({ a: 1 } as any, {\n    get(t, k, r) {\n      if (k === \"g\") {\n        throw new E1(\"get\");\n      }\n      return Reflect.get(t, k, r);\n    },\n  });\n}\n\nasync function rd(p: any): Promise<number> {\n  return p.g;\n}\n\nasync function main() {\n  const rs = await Promise.allSettled([rd(make())]);\n  for (const x of rs) {\n    if (x.status === \"rejected\") {\n      console.log(x.reason.message);\n    }\n  }\n}\n";
    let quiet = "function make(): any {\n  return new Proxy({ a: 1 } as any, {\n    get(t, k, r) {\n      return Reflect.get(t, k, r);\n    },\n  });\n}\n\nasync function rd(p: any): Promise<number> {\n  return p.a;\n}\n\nasync function main() {\n  const rs = await Promise.allSettled([rd(make())]);\n  for (const x of rs) {\n    if (x.status === \"fulfilled\") {\n      console.log(x.value);\n    }\n  }\n}\n";
    for src in [throwing, quiet] {
        let whole = check_in(None, src);
        let two = check_in(Some("2"), src);
        let said = String::from_utf8_lossy(&two.stderr).to_string();
        assert!(whole.status.success(), "{src}");
        assert!(two.status.success(), "{src}:\n{said}");
    }
    // Wrong programs, refused alike: the trap's error is the reason's type, not nothing (a
    // number can't hold an `E1`), and a caught error that is not an `E2`.
    let reason_as_number = throwing.replace(
        "console.log(x.reason.message);",
        "const n: number = x.reason;\n      console.log(n + 1);",
    );
    let caught_as_e2 = reason_as_number.replace(
        "async function main() {\n",
        "class E2 extends Error {}\n\nasync function main() {\n  try {\n    await rd(make());\n  } catch (e) {\n    const x: E2 = e;\n    console.log(x.message);\n  }\n",
    );
    for src in [&reason_as_number, &caught_as_e2] {
        let whole = check_in(None, src);
        let two = check_in(Some("2"), src);
        assert!(!whole.status.success(), "{src}");
        assert_eq!(whole.status.code(), two.status.code(), "{src}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{src}");
    }
    for path in [
        "lang/proxy_throws_reason_one.velt",
        "lang/proxy_throws_reason_two.velt",
        "lang/proxy_throws_catch.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        assert_eq!(whole.status.code(), two.status.code(), "{path}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
    }
}

/// Questions about the whole program are answered from the std code it has
/// (`lazy_std::units_live_set`), not from all of std's unit: whether it reads a `shared` value
/// (`units::app_reads_shared`; a `read` callback passing the store's object to a function value
/// is accepted) and which weak-holding values it makes (`weak_threads`; std's signal code, which
/// a program that never uses it doesn't run, is not reported). Each checks with `VELT_UNITS=2`
/// as by a whole-program check.
#[test]
fn program_wide_answers_come_from_the_std_code_the_program_has() {
    for path in [
        "lang/rwlock_read_closures.velt",
        "lang/rwlock_read_call_result_readers_stop.velt",
        "lang/any_in_builtins.velt",
        "lang/class_objects.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        let said = String::from_utf8_lossy(&two.stderr).to_string();
        assert!(whole.status.success(), "{path}");
        assert!(two.status.success(), "{path}:\n{said}");
    }
}

/// With std checked as a unit of its own, the functions that hand a parameter to a channel
/// (`Channel.send`, `trySend`) come from std's unit, not from reading std's bodies again
/// (`ownership::boundary`): a value owning a resource that is sent while the program still
/// uses it is refused, also through the program's own function that passes it on, as by a
/// whole-program check.
#[test]
fn std_s_unit_keeps_the_functions_that_send_on_a_channel() {
    let path = "lang/errors/channel_send_copies_resource.velt";
    let whole = check_golden_two(path, false);
    let two = check_golden_two(path, true);
    let errors = error_lines(&whole);
    assert!(
        errors
            .iter()
            .any(|l| l.contains("is still used after `send`")),
        "{errors:?}"
    );
    assert_eq!(errors, error_lines(&two));
}

/// A body's edges are kept as written, and its calls through a slot resolved in every check
/// against the program's classes (velt_sema `module_state_facts`): a `with` callback that
/// reaches module state through an override, through an interface's implementation or through
/// a function it calls is refused with that path, by units as by a whole-program check.
#[test]
fn a_call_reaches_the_module_state_the_code_it_may_run_uses() {
    let src = r#"
let count = 0;
class A {
  run(): number {
    return 1;
  }
}
class B extends A {
  override run(): number {
    return count;
  }
}
interface R {
  go(): number;
}
class C implements R {
  go(): number {
    return count;
  }
}
function pick(n: number): A {
  return n > 0 ? new B() : new A();
}
function main() {
  const m = shared(new Mutex<number>(0));
  const a = pick(1);
  const r: R = new C();
  m.with((v) => {
    v += a.run();
  });
  m.with((v) => {
    v += r.go();
  });
  m.with((v) => {
    v += bump();
  });
}
function bump(): number {
  return count++;
}
"#;
    let errors = error_lines(&check_in(None, src));
    for through in ["(through `B.run`)", "(through `C.go`)", "(through `bump`)"] {
        assert!(errors.iter().any(|l| l.contains(through)), "{errors:?}");
    }
    for units in ["1", "2"] {
        assert_eq!(
            errors,
            error_lines(&check_in(Some(units), src)),
            "VELT_UNITS={units}"
        );
    }
}

/// std's unit has no program, so none of its modules is the root whose names are left
/// unqualified: the passes that know std's methods by their `std/` names (a `concat` returns a
/// new array, `ownership::derived`) know them in a check by units too. A `read` callback that
/// changes only the array `concat` made is accepted, as by a whole-program check.
#[test]
fn std_s_unit_names_every_std_method_by_its_module() {
    let src = r#"
class Box {
  items: number[][] = [[1, 2], [3]];
}
function main() {
  const a = new RwLock(new Box());
  a.read((x) => {
    const c = x.items.concat([[5]]);
    c.push([8]);
    console.log(c.length);
  });
}
"#;
    for units in [None, Some("1"), Some("2")] {
        let out = check_in(units, src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "VELT_UNITS={units:?}:\n{stderr}");
    }
}

/// What the source rewrites found per module (a namespace's private members, the modules of a
/// global namespace, the sites of a `globalThis` property) is by the load's module numbers; the
/// app's copy of std's unit holds std's modules first, so those facts are put in its order
/// (`units::remap_module_facts`). Without that a global namespace's members are not found, a
/// namespace's private member is visible, and two modules asserting different types for one
/// `globalThis` property are not told so: each checks with `VELT_UNITS=2` as by a whole-program
/// check.
#[test]
fn the_rewrites_facts_are_in_the_app_s_module_order() {
    for path in [
        "lang/declare_global_namespace_modules.velt",
        "lang/errors/namespace_members.velt",
    ] {
        let whole = check_golden_two(path, false);
        let two = check_golden_two(path, true);
        assert_eq!(whole.status.code(), two.status.code(), "{path}");
        assert_eq!(error_lines(&whole), error_lines(&two), "{path}");
    }
    let lib = r#"
export type Count = number;
export function bump(): number {
  const g = globalThis as { counter?: Count };
  g.counter = (g.counter ?? 0) + 1;
  return g.counter;
}
"#;
    let main = r#"
import { bump } from "./lib";
type Text = string;
function main() {
  const g = globalThis as { counter?: Text };
  console.log(bump(), g.counter);
}
"#;
    let files = [("main.velt", main), ("lib.velt", lib)];
    let errors = error_lines(&check_files(None, &files));
    assert!(
        errors.iter().any(|l| l.contains("`globalThis.counter` is")),
        "{errors:?}"
    );
    assert_eq!(errors, error_lines(&check_files(Some("2"), &files)));
}

/// The error lines of a check's output.
fn error_lines(o: &std::process::Output) -> Vec<String> {
    (String::from_utf8_lossy(&o.stderr).lines())
        .filter(|l| l.contains("error"))
        .map(str::to_string)
        .collect()
}

/// With std checked as a unit of its own (`VELT_UNITS=2`), what std's generic functions need of
/// the types the app gives them comes from std's summary (`JsonNeeds`), not from walking std's
/// bodies again: a value holding a lock that std's `join` prints through its type parameter is
/// refused, as by a whole-program check.
#[test]
fn std_s_summary_gives_the_app_what_std_s_generic_code_needs() {
    let src = r#"
class Guarded {
  m: Mutex<i64[]> = new Mutex<i64[]>([]);
}

function main() {
  const gs = [new Guarded(), new Guarded()];
  console.log(gs.join(", "));
}
"#;
    let whole = check_in(None, src);
    let units = check_in(Some("2"), src);
    assert!(!whole.status.success());
    let errors = error_lines(&whole);
    assert!(
        errors.iter().any(|l| l.contains("without its lock")),
        "{errors:?}"
    );
    assert_eq!(errors, error_lines(&units));
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

/// `velt check` of `src` with std as a unit of its own kept on disk in `disk`.
fn check_on_disk(disk: &std::path::Path, src: &str) -> std::process::Output {
    check_on_disk_with(disk, src, &[])
}

/// [`check_on_disk`] with more of the environment set (`env`: name, value).
fn check_on_disk_with(
    disk: &std::path::Path,
    src: &str,
    env: &[(&str, &std::path::Path)],
) -> std::process::Output {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["check", "main.velt"])
        .current_dir(dir.path())
        .env("VELT_UNITS", "2")
        .env("VELT_UNITS_DISK", disk)
        .env_remove("VELT_UNITS_FREEZE");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

/// What a check said about std's unit on disk: `Some(true)` loaded, `Some(false)` refused,
/// `None` when it read none.
fn from_disk(o: &std::process::Output) -> Option<bool> {
    let said = String::from_utf8_lossy(&o.stderr);
    match () {
        _ if said.contains("std's unit loaded from disk") => Some(true),
        _ if said.contains("std's unit refused from disk") => Some(false),
        _ => None,
    }
}

/// A program that changes what std's stage makes of std (it augments a std interface) neither
/// loads std's unit from disk nor keeps the one its check made (`units::disk`): a unit made
/// with the augmentation would accept the next program's use of the added field, and one made
/// without it would refuse this program's. Both check as by a whole-program check, in either
/// order.
#[test]
fn a_program_that_changes_std_neither_loads_nor_keeps_std_s_unit() {
    let plain = r#"
import type { ParamSink } from "velt/db";
export function f(s: ParamSink): number {
  return s.probeExt ?? 0;
}
function main() {
  console.log("x");
}
"#;
    let block =
        "declare module \"velt/db\" {\n  interface ParamSink {\n    probeExt?: number;\n  }\n}\n";
    let augmenting = plain.replacen("export function", &format!("{block}export function"), 1);
    let whole = check_in(None, &augmenting);
    let said = String::from_utf8_lossy(&whole.stderr).to_string();
    assert!(whole.status.success(), "{said}");
    let refused = error_lines(&check_in(None, plain));
    assert!(
        refused.iter().any(|l| l.contains("probeExt")),
        "{refused:?}"
    );

    let dir = test_dir::TestDir::new();
    let disk = dir.path().join("units");
    // The augmenting program first: nothing is kept.
    let first = check_on_disk(&disk, &augmenting);
    assert!(first.status.success());
    assert_eq!(
        units_in(&disk).len(),
        0,
        "a unit made under an augmentation was kept"
    );
    // The plain program makes and keeps std's unit, and is refused.
    let second = check_on_disk(&disk, plain);
    assert_eq!(refused, error_lines(&second));
    assert_eq!(units_in(&disk).len(), 1);
    // The augmenting program does not load it.
    let third = check_on_disk(&disk, &augmenting);
    let said = String::from_utf8_lossy(&third.stderr).to_string();
    assert_eq!(from_disk(&third), None, "{said}");
    assert!(third.status.success(), "{said}");
    // The plain program loads it, and is still refused.
    let fourth = check_on_disk(&disk, plain);
    assert_eq!(from_disk(&fourth), Some(true));
    assert_eq!(refused, error_lines(&fourth));
}

/// A program with an `any`, whose printing makes std's generator write code for std's own
/// declarations.
const ANY_PROGRAM: &str = r#"
const c = new AbortController();
c.abort("why");
const reason = c.signal.reason;
console.log(reason, typeof reason);
console.log(reason instanceof Error ? "error" : "other");
"#;

/// std's unit holds std's own modules only: the module generated for std's declarations
/// (`<generated>/std/encoding/json/generated/1`), which a check is handed once an earlier one
/// made it, is the program's. So the unit a check makes with a warm build cache serves the
/// checks after it: they are handed the same module apart, which no unit holds.
#[test]
fn std_s_unit_on_disk_does_not_depend_on_the_build_cache() {
    let dir = test_dir::TestDir::new();
    let (disk, cache) = (dir.path().join("units"), dir.path().join("cache"));
    // One program file throughout: what a check generated is offered to the next check of the
    // same program.
    let src = test_dir::TestDir::new();
    std::fs::write(src.path().join("main.velt"), ANY_PROGRAM).unwrap();
    let check = |disk: Option<&std::path::Path>| {
        let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(["check", "main.velt"])
            .current_dir(src.path())
            .env("VELT_UNITS", "2")
            .env("VELT_BUILD_CACHE", &cache)
            .env_remove("VELT_UNITS_FREEZE");
        match disk {
            Some(d) => cmd.env("VELT_UNITS_DISK", d),
            None => cmd.env_remove("VELT_UNITS_DISK"),
        };
        cmd.output().unwrap()
    };
    // A check without the unit on disk warms the build cache with what it generated.
    assert!(check(None).status.success());
    let first = check(Some(&disk));
    let said = String::from_utf8_lossy(&first.stderr).to_string();
    assert!(first.status.success(), "{said}");
    assert_eq!(from_disk(&first), None, "{said}");
    for run in 2..=3 {
        let next = check(Some(&disk));
        let said = String::from_utf8_lossy(&next.stderr).to_string();
        assert!(next.status.success(), "run {run}:\n{said}");
        assert_eq!(from_disk(&next), Some(true), "run {run}:\n{said}");
    }
    assert_eq!(units_in(&disk).len(), 1);
}

/// A changed std is another unit: the key std's unit is kept under covers std's sources, so a
/// check after an edit of one std file reads no unit, checks std again and keeps a second one.
#[test]
fn a_changed_std_does_not_load_the_unit_of_the_std_before() {
    let dir = test_dir::TestDir::new();
    let (disk, std) = (dir.path().join("units"), dir.path().join("std"));
    let repo_std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    copy_dir(&repo_std, &std);
    let env = [("VELT_STD", std.as_path())];
    let ok = "function main() {\n  console.log(\"hi\");\n}\n";
    let first = check_on_disk_with(&disk, ok, &env);
    let said = String::from_utf8_lossy(&first.stderr).to_string();
    assert!(first.status.success(), "{said}");
    assert_eq!(from_disk(&check_on_disk_with(&disk, ok, &env)), Some(true));
    let file = std.join("fs/index.velt");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n// edited\n");
    std::fs::write(&file, text).unwrap();
    let after = check_on_disk_with(&disk, ok, &env);
    let said = String::from_utf8_lossy(&after.stderr).to_string();
    assert!(after.status.success(), "{said}");
    assert_eq!(from_disk(&after), None, "{said}");
    assert_eq!(units_in(&disk).len(), 2);
}

/// Copy the directory `from` to `to`, with everything in it.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_dir(&entry.path(), &target),
            false => {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
}

/// The units kept in `disk`.
fn units_in(disk: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = (std::fs::read_dir(disk).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "unit"))
        .collect();
    out.sort();
    out
}

/// std's unit kept on disk (`VELT_UNITS_DISK`) by one program's check serves another's: it
/// is loaded instead of checking std, and the program is refused with the errors of a
/// whole-program check. A damaged unit is refused (its checksum), std is checked again, and the
/// unit is saved again.
#[test]
fn std_s_unit_on_disk_serves_another_program() {
    let dir = test_dir::TestDir::new();
    let disk = dir.path().join("units");
    let first = check_on_disk(&disk, PROGRAM);
    let said = String::from_utf8_lossy(&first.stderr).to_string();
    assert!(first.status.success(), "{said}");
    assert!(
        !said.contains("from disk"),
        "nothing was on disk yet:\n{said}"
    );
    let saved = units_in(&disk);
    assert_eq!(saved.len(), 1, "std's unit was saved: {saved:?}");
    let bad = "function main() {\n  const n: number = \"text\";\n  console.log(n);\n}\n";
    let whole = check_in(None, bad);
    assert!(!whole.status.success());
    let second = check_on_disk(&disk, bad);
    let said = String::from_utf8_lossy(&second.stderr).to_string();
    assert!(said.contains("std's unit loaded from disk"), "{said}");
    assert_eq!(whole.status.code(), second.status.code());
    assert_eq!(error_lines(&whole), error_lines(&second));
    // A byte of the unit changed: refused, std checked again, and the unit saved again.
    let mut bytes = std::fs::read(&saved[0]).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x5a;
    std::fs::write(&saved[0], &bytes).unwrap();
    let third = check_on_disk(&disk, bad);
    let said = String::from_utf8_lossy(&third.stderr).to_string();
    assert!(said.contains("std's unit refused from disk"), "{said}");
    assert_eq!(error_lines(&whole), error_lines(&third));
    let again = std::fs::read(&saved[0]).unwrap();
    assert_ne!(again[at], bytes[at], "the damaged unit was saved again");
}

/// `get message()` on a class extending `Error` (#3792): a whole-program check reads every
/// `message` of an error through it, but a check by units checks std's bodies (`toString`,
/// `stack`) before the program's classes are known, so both modes refuse it rather than give
/// those reads another answer.
#[test]
fn a_get_message_override_of_error_is_refused_by_units() {
    let src = "class E extends Error {\n  get message(): string {\n    return \"g\";\n  }\n}\nconsole.log(String(new E()));\n";
    let whole = check_in(None, src);
    assert!(
        whole.status.success(),
        "{}",
        String::from_utf8_lossy(&whole.stderr)
    );
    for units in ["1", "2"] {
        let out = check_in(Some(units), src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "VELT_UNITS={units}: accepted\n{stderr}"
        );
        let errors: Vec<&str> = stderr.lines().filter(|l| l.contains("error:")).collect();
        assert_eq!(errors.len(), 1, "VELT_UNITS={units}:\n{stderr}");
        assert!(
            errors[0].contains(
                "`get message()` on a class extending `Error` is not supported in a check by units"
            ),
            "VELT_UNITS={units}:\n{stderr}"
        );
    }
}
