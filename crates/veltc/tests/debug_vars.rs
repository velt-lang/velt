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
