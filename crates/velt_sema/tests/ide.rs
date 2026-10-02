//! The editor query API (`velt_sema::ide`, docs/internals/contracts/sema_ide.md).

mod common;

use common::programs::{load_src, load_src_at, load_src_lenient, repo_root};
use velt_common::FileId;
use velt_sema::ide::{check_for_ide, Analysis, DefKind};

/// Analysis of an inline program, its root file and source.
fn analyze(src: &str) -> (Analysis, FileId) {
    let l = load_src(src);
    let file = l.modules[l.root].file;
    (check_for_ide(&l.modules, l.root), file)
}

/// Byte offset of the `n`th (0-based) occurrence of `needle` in `src`, plus `delta`.
fn at(src: &str, needle: &str, n: usize, delta: u32) -> u32 {
    let mut from = 0;
    for _ in 0..n {
        from += src[from..].find(needle).expect("needle") + needle.len();
    }
    (from + src[from..].find(needle).expect("needle")) as u32 + delta
}

const PROGRAM: &str = "struct Point { x: i64; y: i64; }
class User {
  name: string;
  constructor(name: string) { this.name = name; }
  greet(greeting: string): string { return `${greeting} ${this.name}`; }
  get shout(): string { return this.name; }
}
function make(n: i64): Point { return Point { x: n, y: 2 }; }
function main() {
  const p = make(1);
  let total = p.x + p.y;
  const u = new User(\"ann\");
  console.log(u.greet(\"hi\"), total, u.shout);
  const m = new Map<string, i64>();
  m.set(\"a\", total);
  const f = (z: i64) => `${z}`;
  console.log(f(1), Math.PI, m.size);
}
";

#[test]
fn def_at_resolves_uses_and_declarations() {
    let (a, file) = analyze(PROGRAM);
    let d = a.def_at(file, at(PROGRAM, "make(1)", 0, 1)).expect("make");
    assert_eq!((d.name.as_str(), d.kind), ("make", DefKind::Function));
    assert_eq!(d.span.lo, at(PROGRAM, "make(n", 0, 0));
    assert_eq!(d.detail, "function make(n: i64): Point");
    let x = a.def_at(file, at(PROGRAM, "p.x", 0, 2)).expect("field");
    assert_eq!((x.name.as_str(), x.kind), ("x", DefKind::Field));
    assert_eq!(x.detail, "(field) Point.x: i64");
    let g = a
        .def_at(file, at(PROGRAM, "greet(\"hi\")", 0, 0))
        .expect("method");
    assert_eq!(g.kind, DefKind::Method);
    assert_eq!(g.detail, "(method) User.greet(greeting: string): string");
    let s = a
        .def_at(file, at(PROGRAM, "u.shout", 0, 3))
        .expect("getter");
    assert_eq!(s.kind, DefKind::Getter);
    let t = a
        .def_at(file, at(PROGRAM, "total, u", 0, 0))
        .expect("local");
    assert_eq!(
        (t.kind, t.detail.as_str()),
        (DefKind::Local, "let total: i64")
    );
    assert_eq!(t.span.lo, at(PROGRAM, "total =", 0, 0));
    let own = a
        .def_at(file, at(PROGRAM, "total =", 0, 0))
        .expect("declaration");
    assert!(own.same_def(&t));
    let ty = a
        .def_at(file, at(PROGRAM, "Point {", 1, 0))
        .expect("struct");
    assert_eq!(ty.kind, DefKind::Struct);
    let pi = a.def_at(file, at(PROGRAM, "PI", 0, 0)).expect("static");
    assert_eq!(pi.kind, DefKind::StaticField);
    let prelude = a
        .def_at(file, at(PROGRAM, "set(", 0, 0))
        .expect("prelude method");
    assert_ne!(prelude.module, 0, "declared in the prelude");
}

#[test]
fn type_at_shows_source_names() {
    let (a, file) = analyze(PROGRAM);
    assert_eq!(
        a.type_at(file, at(PROGRAM, "p = ", 0, 0)).as_deref(),
        Some("Point")
    );
    assert_eq!(
        a.type_at(file, at(PROGRAM, "m = ", 0, 0)).as_deref(),
        Some("Map<string, i64>")
    );
    assert_eq!(
        a.type_at(file, at(PROGRAM, "u.greet", 0, 0)).as_deref(),
        Some("User")
    );
    assert_eq!(
        a.type_at(file, at(PROGRAM, "f(1)", 0, 0)).as_deref(),
        Some("(z: i64) => string")
    );
    assert_eq!(
        a.type_at(file, at(PROGRAM, "p.x + p.y", 0, 2)).as_deref(),
        Some("i64")
    );
}

#[test]
fn scope_at_lists_visible_names() {
    let (a, file) = analyze(PROGRAM);
    let names: Vec<String> = a
        .scope_at(file, at(PROGRAM, "console.log(u.greet", 0, 0))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for n in [
        "p", "total", "u", "make", "Point", "User", "main", "Map", "Math",
    ] {
        assert!(names.contains(&n.to_string()), "{n} missing: {names:?}");
    }
    assert!(
        !names.contains(&"m".to_string()),
        "declared later in the block"
    );
    let in_make: Vec<String> = a
        .scope_at(file, at(PROGRAM, "Point { x: n", 0, 0))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(in_make.contains(&"n".to_string()));
    assert!(!in_make.contains(&"total".to_string()));
}

#[test]
fn members_of_values_and_types() {
    let (a, file) = analyze(PROGRAM);
    let ms = a.members_of_type_at(file, at(PROGRAM, "u.greet", 0, 0));
    let names: Vec<&str> = ms.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["name", "greet", "shout"]);
    let greet = ms.iter().find(|(n, _, _)| n == "greet").expect("greet");
    assert_eq!(greet.2, "(greeting: string) => string");
    let map = a.members_of_type_at(file, at(PROGRAM, "m.set", 0, 0));
    let set = map.iter().find(|(n, _, _)| n == "set").expect("Map.set");
    assert_eq!(set.2, "(key: string, value: i64) => void");
    assert!(map.iter().any(|(n, _, t)| n == "size" && t == "usize"));
    assert!(
        !map.iter().any(|(n, _, _)| n == "slots"),
        "private members are hidden"
    );
    let math = a.members_of_type_at(file, at(PROGRAM, "Math.PI", 0, 0));
    assert!(math.iter().any(|(n, _, t)| n == "PI" && t == "f64"));
    assert!(math.iter().any(|(n, _, _)| n == "sqrt"));
    let local = a.def_at(file, at(PROGRAM, "p = ", 0, 0)).expect("p");
    let fields: Vec<String> = a
        .members_of(&local)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert_eq!(fields, ["x", "y"]);
    let arr = analyze("function main() { const xs = [1, 2]; console.log(xs.length); }");
    let off = at(
        "function main() { const xs = [1, 2]; console.log(xs.length); }",
        "xs.length",
        0,
        0,
    );
    let arr_members = arr.0.members_of_type_at(arr.1, off);
    assert!(
        arr_members
            .iter()
            .any(|(n, _, t)| n == "map" && t.starts_with("(f: (arg0: i64) => U throws E)")),
        "{arr_members:?}"
    );
}

#[test]
fn references_cover_every_use() {
    let (a, file) = analyze(PROGRAM);
    let total = a.def_at(file, at(PROGRAM, "total =", 0, 0)).expect("total");
    let spans = a.references(&total);
    assert_eq!(spans.len(), 3, "declaration + 2 uses");
    assert!(spans
        .iter()
        .all(|s| &PROGRAM[s.lo as usize..s.hi as usize] == "total"));
    let name = a
        .def_at(file, at(PROGRAM, "name: string;", 0, 0))
        .expect("field");
    let uses: Vec<&str> = a
        .references(&name)
        .iter()
        .map(|s| &PROGRAM[s.lo as usize..s.hi as usize])
        .collect();
    assert_eq!(
        uses.len(),
        4,
        "declaration, ctor assignment, greet, shout: {uses:?}"
    );
    let point = a.def_at(file, at(PROGRAM, "Point {", 0, 0)).expect("Point");
    assert_eq!(a.references(&point).len(), 3);
}

#[test]
fn works_with_errors_and_without_main() {
    let src = "function f(a: i64): i64 { return a + \"x\"; }
function g(): i64 { const b = 2; return b; }";
    let (a, file) = analyze(src);
    assert!(a.diagnostics().iter().any(|d| d.is_error()));
    assert!(!a.diagnostics().iter().any(|d| d.message.contains("main")));
    let b = a
        .def_at(file, at(src, "b;", 0, 0))
        .expect("g is checked despite f's error");
    assert_eq!(b.detail, "const b: i64");
    let broken = "function main() { const x = 1; x. }";
    let l = load_src_lenient(broken);
    let an = check_for_ide(&l.modules, l.root);
    let names: Vec<String> = an
        .scope_at(l.modules[l.root].file, at(broken, "x.", 0, 0))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"main".to_string()));
}

#[test]
fn nested_items_interfaces_and_imports() {
    let src = "interface Named { name: string; hello(): string { return this.name; } }
class Dog implements Named { name: string = \"rex\"; }
function main() {
  function helper(d: Dog): string { return d.hello(); }
  console.log(helper(new Dog()));
}";
    let (a, file) = analyze(src);
    let h = a
        .def_at(file, at(src, "helper(new", 0, 0))
        .expect("nested fn");
    assert_eq!(h.span.lo, at(src, "helper(d", 0, 0));
    let hello = a
        .def_at(file, at(src, "hello()", 1, 0))
        .expect("default method");
    assert_eq!(hello.span.lo, at(src, "hello()", 0, 0));
    let scope: Vec<String> = a
        .scope_at(file, at(src, "console", 0, 0))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(scope.contains(&"helper".to_string()));
}

#[test]
fn hover_shows_what_a_function_throws() {
    let src = "class NotFound extends Error {}
class Forbidden extends Error {}
function load(id: string): string { if (id == \"\") throw new NotFound(id); throw new Forbidden(id); }
function safe(): i64 { return 1; }
function main() { try { load(\"x\"); } catch (e) { console.log(e.message); } console.log(safe()); }";
    let (a, file) = analyze(src);
    let load = a.def_at(file, at(src, "load(\"x\")", 0, 0)).expect("load");
    assert_eq!(
        load.detail,
        "function load(id: string): string throws NotFound | Forbidden"
    );
    let safe = a.def_at(file, at(src, "safe()", 1, 0)).expect("safe");
    assert_eq!(safe.detail, "function safe(): i64");
}

#[test]
fn jsx_tags_and_attributes() {
    let src = "// @jsxImportSource ./_jsx_test_provider
function main() { const e = <a href=\"/x\">x</a>; }";
    let l = load_src_at(&repo_root().join("tests/golden/lang/main.vlt"), src);
    let file = l.modules[l.root].file;
    let a = check_for_ide(&l.modules, l.root);
    let tags = a.jsx_intrinsics(file);
    let names: Vec<&str> = tags.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(
        names.starts_with(&["a", "br", "button", "div"]),
        "{names:?}"
    );
    let (_, anchor, ty) = &tags[0];
    assert_eq!(anchor.kind, DefKind::Field);
    assert_eq!(
        ty,
        "{ href: string | null; class: string | null; title: string | null }"
    );
    // The tag in the source names the same field, and its attributes are the members.
    let used = a.def_at(file, at(src, "<a", 0, 1)).expect("tag");
    assert!(used.same_def(anchor));
    let attrs: Vec<(String, String)> = a
        .members_of(anchor)
        .into_iter()
        .map(|(n, _, t)| (n, t))
        .collect();
    assert_eq!(attrs[0], ("href".to_string(), "string | null".to_string()));
    assert_eq!(attrs.len(), 3);
    // A file without JSX has no tags.
    let (plain, plain_file) = analyze("function main() {}");
    assert!(plain.jsx_intrinsics(plain_file).is_empty());
}

#[test]
fn members_of_a_field_are_the_members_of_its_type() {
    let src = "struct Inner { a: i64; b: string; }
struct Outer { inner: Inner; }
function main() { const o = Outer { inner: Inner { a: 1, b: \"x\" } }; console.log(o.inner.a); }";
    let (a, file) = analyze(src);
    let inner = a.def_at(file, at(src, "inner.a", 0, 0)).expect("field");
    let names: Vec<String> = a
        .members_of(&inner)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert_eq!(names, ["a", "b"]);
}
