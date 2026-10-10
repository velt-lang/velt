//! Debug descriptions of source variables (vir.rs invariant 10), from real sources: debug builds
//! made for debugging (`BuildOptions::debug_vars`: `velt build`, `velt dev --exe`) describe each
//! function's params and named locals with their source types; builds that only run and release
//! builds describe nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;

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

fn compile(release: bool) -> Program {
    compile_with(release, true)
}

fn compile_with(release: bool, debug_vars: bool) -> Program {
    let opts = BuildOptions {
        input: PathBuf::from("main.vlt"),
        root_source: Some(SRC.to_string()),
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
