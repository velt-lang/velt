//! Debug descriptions of source variables (vir.rs invariant 10), from real sources: debug builds
//! made for debugging (`BuildOptions::debug_vars`: `velt build`, `velt dev --exe`) describe each
//! function's params and named locals with their source types; builds that only run and release
//! builds describe nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use velt_vir::vir::{DebugKind, Program, Ty};
use veltc::driver::{self, BuildOptions, Session};

const SRC: &str = r#"class Point {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}

enum Color {
  Red,
  Green = 5,
}

function area(w: number, h: number): number {
  const a = w * h;
  return a;
}

function main() {
  const name = "velt";
  const xs: number[] = [1, 2, 3];
  const p = new Point(1, 2);
  const maybe: Point | null = null;
  const c = Color.Green;
  const t: [string, boolean] = ["a", true];
  let count: i64 = 0;
  for (const x of xs) {
    count += 1;
  }
  const v: string | number = 1.5;
  let n = 0;
  while (n < 10) {
    n += 1;
  }
  console.log(name, xs.length, p.x, maybe == null, c, t[0], count, n, v, area(2, 3));
}
"#;

/// Types that contain themselves, names a user may write and sema's temporaries, closures.
const MORE: &str = r#"interface Item {
  name: string;
  children: Item[];
}

type Node = { value: number; next: Node | null };

function main() {
  const tree: Item = { name: "root", children: [] };
  const list: Node = { value: 1, next: null };
  const __mine = 3;
  const label: string | null = null;
  const shown = label ?? "none";
  const base = 10;
  const addBase = (x: number): number => x + base;
  const fns: ((x: number) => number)[] = [addBase];
  console.log(tree.name, list.value, __mine, shown, fns.length, addBase(1));
}
"#;

fn compile(release: bool) -> Program {
    compile_with(release, true)
}

fn compile_with(release: bool, debug_vars: bool) -> Program {
    compile_source(SRC, release, debug_vars)
}

fn compile_source(src: &str, release: bool, debug_vars: bool) -> Program {
    let opts = BuildOptions {
        input: PathBuf::from("main.vlt"),
        root_source: Some(src.to_string()),
        release,
        debug_vars,
        // An object: whatever links it keeps the line information variables need (on Windows
        // an executable linked by `link.exe` has none).
        emit: veltc::cli::Emit::Obj,
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    driver::compile(&mut sess, &opts)
        .unwrap_or_else(|e| panic!("{e:?}\n{}", sess.render_diagnostics()))
}

/// The described variables of the function whose symbol ends with `name`: name → (debug
/// type name, declaration line, param).
fn vars(p: &Program, name: &str) -> BTreeMap<String, (String, u32, bool)> {
    let f = p
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with(name))
        .unwrap_or_else(|| panic!("no function {name}"));
    f.locals
        .iter()
        .filter_map(|l| {
            let d = l.debug.as_ref()?;
            let ty = p.debug_ty(d.ty).name.clone();
            Some((l.name.clone()?, (ty, d.decl.line, d.param)))
        })
        .collect()
}

#[test]
fn debug_builds_describe_params_and_locals() {
    let p = compile(false);
    velt_vir::verify(&p).unwrap();
    let area = vars(&p, "4area");
    assert_eq!(area.keys().collect::<Vec<_>>(), ["a", "h", "w"], "{area:?}");
    assert_eq!(area["w"], ("number".into(), 15, true));
    assert!(!area["a"].2);
    let main = vars(&p, "4main");
    let ty = |n: &str| main[n].0.as_str();
    assert_eq!(ty("name"), "string");
    assert_eq!(ty("xs"), "number[]");
    assert_eq!(ty("p"), "Point");
    assert_eq!(ty("maybe"), "Point | null");
    assert_eq!(ty("c"), "Color");
    assert_eq!(ty("t"), "[string, boolean]");
    assert_eq!(ty("count"), "i64");
    assert_eq!(ty("x"), "number");
    assert_eq!(main["xs"].1, 22);
}

#[test]
fn debug_types_follow_the_layouts() {
    let p = compile(false);
    let by_name = |n: &str| {
        p.debug_types
            .iter()
            .find(|t| t.name == n)
            .unwrap_or_else(|| panic!("no debug type {n}"))
    };
    let DebugKind::Class { obj, fields } = &by_name("Point").kind else {
        panic!("Point is not a class")
    };
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["x", "y"]);
    let obj = p.agg(*obj);
    let offsets: Vec<u32> = fields
        .iter()
        .map(|f| obj.fields[f.index as usize].1)
        .collect();
    assert_eq!(offsets[1] - offsets[0], 8);
    let DebugKind::Enum { members } = &by_name("Color").kind else {
        panic!("Color is not an enum")
    };
    assert_eq!(members, &[("Red".into(), 0), ("Green".into(), 5)]);
    let DebugKind::Option { repr, inner } = &by_name("Point | null").kind else {
        panic!("not an option")
    };
    assert_eq!(
        (*repr, p.debug_ty(*inner).name.as_str()),
        (Ty::Ptr, "Point")
    );
    // A union's variants are its members, by their source names.
    let DebugKind::Tagged { variants, .. } = &by_name("string | number").kind else {
        panic!("not a tagged type")
    };
    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["string", "number"]);
    let DebugKind::Array { elem } = &by_name("number[]").kind else {
        panic!("not an array")
    };
    assert_eq!(p.debug_ty(*elem).kind, DebugKind::Scalar(Ty::F64));
    assert_eq!(by_name("string").kind, DebugKind::Str);
}

#[test]
fn block_variables_end_with_their_block() {
    // `x` (the `for … of` binding) ends with the loop body; `count`, declared in the function's
    // own block, and the params end with the function.
    let p = compile(false);
    let main = p
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("4main"))
        .unwrap();
    let end_of = |name: &str| {
        main.locals
            .iter()
            .find(|l| l.name.as_deref() == Some(name) && l.debug.is_some())
            .and_then(|l| l.debug.as_ref().unwrap().end)
    };
    let x = end_of("x").expect("x ends with the loop body");
    assert_eq!((x.line, x.col), (30, 4), "{x:?}");
    assert_eq!(end_of("count"), None);
    let area = p
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("4area"))
        .unwrap();
    assert!(area
        .locals
        .iter()
        .filter_map(|l| l.debug.as_ref())
        .all(|d| d.end.is_none()));
}

#[test]
fn a_narrowed_number_stays_described() {
    // `numrep` stores the counter `n` as an integer; the description moves with it.
    let p = compile(false);
    let main = p
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("4main"))
        .unwrap();
    let described: Vec<Ty> = main
        .locals
        .iter()
        .filter(|l| l.name.as_deref() == Some("n") && l.debug.is_some())
        .map(|l| l.ty)
        .collect();
    assert_eq!(described.len(), 1, "{described:?}");
    assert_ne!(
        described[0],
        Ty::F64,
        "n was not narrowed; the test needs another counter"
    );
    assert_eq!(vars(&p, "4main")["n"].0, "number");
}

#[test]
fn builds_that_only_run_describe_nothing() {
    // `velt run`, `velt test`: variables in memory would slow them for nothing.
    let p = compile_with(false, false);
    assert!(p.debug_types.is_empty());
    assert!(p
        .funcs
        .iter()
        .all(|f| f.locals.iter().all(|l| l.debug.is_none())));
}

#[test]
fn release_builds_describe_nothing() {
    let p = compile(true);
    assert!(p.debug_types.is_empty());
    assert!(p
        .funcs
        .iter()
        .all(|f| f.locals.iter().all(|l| l.debug.is_none())));
}

#[test]
fn types_that_contain_themselves_are_named_once() {
    let p = compile_source(MORE, false, true);
    velt_vir::verify(&p).unwrap();
    let main = vars(&p, "4main");
    let ty = |n: &str| main[n].0.as_str();
    assert_eq!(ty("tree"), "{ name: string; children: { … }[] }");
    assert_eq!(ty("list"), "{ value: number; next: { … } | null }");
    assert_eq!(ty("fns"), "function[]");
}

#[test]
fn sema_temporaries_are_hidden_and_user_names_are_not() {
    let p = compile_source(MORE, false, true);
    let names: Vec<String> = p
        .funcs
        .iter()
        .flat_map(|f| f.locals.iter())
        .filter(|l| l.debug.is_some())
        .filter_map(|l| l.name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "__mine"), "{names:?}");
    assert!(
        names
            .iter()
            .all(|n| n.starts_with(|c: char| c.is_alphabetic() || c == '_' || c == '$')),
        "{names:?}"
    );
}

#[test]
fn closure_captures_are_not_params() {
    let p = compile_source(MORE, false, true);
    let closure = p
        .funcs
        .iter()
        .find(|f| {
            f.symbol.contains("closure")
                && f.locals.iter().any(|l| l.name.as_deref() == Some("base"))
        })
        .expect("the closure");
    let vars = vars(&p, &closure.symbol);
    assert!(vars["x"].2, "{vars:?}");
    assert!(!vars["base"].2, "{vars:?}");
}

/// Every golden program that compiles also compiles with variables described, and its VIR
/// verifies (the golden suite builds with `velt run`, which describes none).
#[test]
fn every_golden_compiles_with_variables() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    let mut programs = vec![];
    for category in std::fs::read_dir(&root).unwrap().flatten() {
        let Ok(entries) = std::fs::read_dir(category.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if path.extension().is_some_and(|x| x == "vlt") && !name.starts_with('_') {
                programs.push(path);
            }
        }
    }
    programs.sort();
    assert!(programs.len() > 100, "found {} goldens", programs.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(vec![]);
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(4, |n| n.get()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(path) = programs.get(i) else { break };
                let opts = BuildOptions {
                    input: path.clone(),
                    debug_vars: true,
                    emit: veltc::cli::Emit::Obj,
                    ..BuildOptions::default()
                };
                let mut sess = Session::new();
                let problem = match driver::compile(&mut sess, &opts) {
                    Ok(p) => velt_vir::verify(&p).err().map(|e| e.join("; ")),
                    Err(driver::BuildError::Ice(msg)) => Some(msg),
                    // Programs that don't compile (expected errors, packages) are not this test's.
                    Err(_) => None,
                };
                if let Some(problem) = problem {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("{}: {problem}", path.display()));
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Blocks lowered outside `block`: a `catch` body and a `switch` case.
const HANDLERS: &str = r#"class Oops extends Error {}

function risky(n: number): number throws Oops {
  if (n > 1) {
    throw new Oops("big");
  }
  return n;
}

function main() {
  let total = 0;
  try {
    total = risky(2);
  } catch (e) {
    const inCatch = 7;
    total = inCatch;
  } finally {
    total += 1;
  }
  const k = 2;
  switch (k) {
    case 2:
      const two = k * 2;
      total += two;
      break;
    default:
      total += 0;
  }
  console.log(total);
}
"#;

#[test]
fn catch_bodies_and_switch_cases_end_their_variables() {
    let p = compile_source(HANDLERS, false, true);
    velt_vir::verify(&p).unwrap();
    let main = p
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("4main"))
        .unwrap();
    let end_of = |name: &str| {
        main.locals
            .iter()
            .find(|l| l.name.as_deref() == Some(name) && l.debug.is_some())
            .unwrap_or_else(|| panic!("no {name}"))
            .debug
            .as_ref()
            .unwrap()
            .end
    };
    let line = |text: &str| HANDLERS.lines().position(|l| l.contains(text)).unwrap() as u32 + 1;
    // `inCatch` ends with the `catch` body, before `finally`; `two` with its case, before the
    // `switch` ends.
    let in_catch = end_of("inCatch").expect("inCatch ends");
    assert_eq!(in_catch.line, line("} finally {"), "{in_catch:?}");
    let two = end_of("two").expect("two ends");
    assert!(two.line < line("console.log"), "{two:?}");
    assert_eq!(end_of("total"), None);
}

/// `llvm-dwarfdump` (`dwarfdump` on macOS), if installed.
fn dwarfdump() -> Option<&'static str> {
    ["llvm-dwarfdump", "dwarfdump"].into_iter().find(|tool| {
        std::process::Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// The debug info of programs with blocks lowered outside `block` (`catch` bodies, `switch`
/// cases, JSX components) passes `dwarfdump --verify`: no lexical block overlaps a sibling
/// (regression: a `catch` body's variables lasted to the function's end).
#[test]
fn dwarf_of_handlers_and_cases_verifies() {
    let Some(tool) = dwarfdump() else {
        eprintln!("note: no llvm-dwarfdump; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let goldens = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    let mut programs: Vec<(String, BuildOptions)> = vec![(
        "handlers".into(),
        BuildOptions {
            input: dir.path().join("handlers.vlt"),
            root_source: Some(HANDLERS.to_string()),
            ..BuildOptions::default()
        },
    )];
    for golden in [
        "lang/field_default_generic_throws",
        "lang/field_default_generic_siblings",
        "std/jsx_async",
        "std/jsx_render",
    ] {
        let input = goldens.join(format!("{golden}.vlt"));
        programs.push((
            golden.replace('/', "_"),
            BuildOptions {
                input,
                ..BuildOptions::default()
            },
        ));
    }
    for (name, opts) in programs {
        let object = dir.path().join(format!("{name}.o"));
        let opts = BuildOptions {
            output: Some(object.clone()),
            emit: veltc::cli::Emit::Obj,
            debug_vars: true,
            ..opts
        };
        let mut sess = Session::new();
        driver::build(&mut sess, &opts)
            .unwrap_or_else(|e| panic!("{name}: {e:?}\n{}", sess.render_diagnostics()));
        let out = std::process::Command::new(tool)
            .arg("--verify")
            .arg(&object)
            .output()
            .expect("dwarfdump");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name}: {text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A Windows executable that MSVC's `link.exe` links has no line information, so it describes
/// no variables either (regression: they named the files the driver had dropped, an ICE in
/// every debug `velt build` on such a machine). Here the bundled linker has no Windows kit.
#[test]
fn a_build_without_line_information_describes_no_variables() {
    let opts = BuildOptions {
        input: PathBuf::from("main.vlt"),
        root_source: Some(SRC.to_string()),
        target: Some("x86_64-pc-windows-msvc".into()),
        debug_vars: true,
        ..BuildOptions::default()
    };
    if opts.has_line_info() {
        eprintln!("note: this machine links Windows programs with the bundled lld; skipping");
        return;
    }
    let mut sess = Session::new();
    let p = driver::compile(&mut sess, &opts)
        .unwrap_or_else(|e| panic!("{e:?}\n{}", sess.render_diagnostics()));
    velt_vir::verify(&p).unwrap();
    assert!(p.files.is_empty());
    assert!(p.debug_types.is_empty());
}
