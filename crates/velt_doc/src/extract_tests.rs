//! Unit tests of [`super`]: what is extracted from a module's source.

use super::*;

const SRC: &str = "// Module docs.\n// Second line.\n\nimport { x } from \"velt/io\";\n\n/// Adds.\n/// Twice.\nexport function add(a: i64, b: i64): i64 {\n  return a + b;\n}\n\nfunction hidden() {}\n\n/** A point. */\nexport class Point {\n  /** X coordinate. */\n  x: f64;\n  private secret: i64 = 0;\n\n  constructor(x: f64) {\n    this.x = x;\n  }\n\n  /**\n   * Length.\n   */\n  len(): f64 {\n    return this.x;\n  }\n}\n\nexport enum Color {\n  Red,\n  Green = 5,\n}\n\nexport const LIMIT: i64 = 10;\n\nexport async function wait(ms: i64) {}\n";

#[test]
fn exported_items_with_docs() {
    let m = extract("demo", SRC);
    assert_eq!(m.doc, "Module docs.\nSecond line.");
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["add", "Point", "Color", "LIMIT", "wait"]);
    let add = &m.items[0];
    assert_eq!(add.signature, "function add(a: i64, b: i64): i64");
    assert_eq!(add.doc, "Adds.\nTwice.");
    let point = &m.items[1];
    assert_eq!(
        (point.kind, point.signature.as_str()),
        (Kind::Class, "class Point")
    );
    let members: Vec<(&str, &str, &str)> = point
        .members
        .iter()
        .map(|m| (m.name.as_str(), m.signature.as_str(), m.doc.as_str()))
        .collect();
    assert_eq!(
        members,
        [
            ("x", "x: f64", "X coordinate."),
            ("constructor", "constructor(x: f64)", ""),
            ("len", "len(): f64", "Length.")
        ]
    );
    assert_eq!(m.items[2].members.len(), 2);
    assert_eq!(m.items[3].signature, "const LIMIT: i64 = 10");
    assert_eq!(m.items[4].signature, "async function wait(ms: i64)");
}

#[test]
fn tags_and_deprecation() {
    let src = "/**\n * Old.\n * @param a - the value\n * @deprecated Use `g`.\n */\nexport function f(a: i64) {}\n\nexport enum E {\n  /** @deprecated */\n  A,\n  // Plain.\n  B,\n}\n";
    let m = extract("demo", src);
    let f = &m.items[0];
    assert_eq!(
        f.doc,
        "Old.\n\n**Parameters**\n\n- `a`: the value\n\n**Deprecated:** Use `g`."
    );
    assert_eq!(f.deprecated.as_deref(), Some("Use `g`."));
    let e = &m.items[1].members;
    assert_eq!(e[0].deprecated.as_deref(), Some(""));
    assert_eq!((e[1].doc.as_str(), e[1].deprecated.as_ref()), ("", None));
}

#[test]
fn overloads_show_their_signatures() {
    let src = "/** Twice. */\nexport function f(x: string): string;\nexport async function f(x: number): Promise<number>;\nexport async function f(x: string | number): Promise<string | number> {\n  return x;\n}\n\nexport class C {\n  /** Scales. */\n  m(a: f64): f64;\n  m(a: f64, b: f64): f64;\n  m(a: f64, b?: f64): f64 {\n    return a;\n  }\n  static m2(): C;\n  static m2(): C {\n    return new C();\n  }\n}\n";
    let m = extract("demo", src);
    let f = &m.items[0];
    assert_eq!(
        f.signature,
        "function f(x: string): string\nasync function f(x: number): Promise<number>"
    );
    assert_eq!(f.doc, "Twice.");
    let members: Vec<&str> = m.items[1]
        .members
        .iter()
        .map(|m| m.signature.as_str())
        .collect();
    assert_eq!(
        members,
        ["m(a: f64): f64\nm(a: f64, b: f64): f64", "static m2(): C"]
    );
    assert_eq!(m.items[1].members[0].doc, "Scales.");
}

/// Decorators are part of the API: every target shows its own, as written, before its
/// modifiers, and a doc comment above them documents the declaration.
#[test]
fn every_target_shows_its_decorators() {
    let src = r#"export declare decorator function dd(k: string = "a"): Repeatable<TypeDecorator>;
/** A class. */
@dd @dd("c")
export class C {
  /** A field. */
  @dd("x") x: i64 = 0;
  @dd static readonly s: i64 = 1;
  constructor(@dd public p: i64) {}
  /** A method. */
  @dd("m") static m(@dd("p") a: i64): i64 {
return a;
  }
  o(a: string): void;
  @dd("o") o(a: any) {}
}
/** A function. */
@dd
export function f(@dd a: i64) {}
@dd("e")
export enum E {
  A,
}
export function g(a: string): void;
@dd("g")
export function g(a: any) {}
"#;
    let m = extract("demo", src);
    let sigs: Vec<&str> = m.items.iter().map(|i| i.signature.as_str()).collect();
    assert_eq!(
        sigs,
        [
            "declare decorator function dd(k: string = \"a\"): Repeatable<TypeDecorator>",
            "@dd @dd(\"c\") class C",
            "@dd function f(@dd a: i64)",
            "@dd(\"e\") enum E",
            "@dd(\"g\") function g(a: string): void",
        ]
    );
    let members: Vec<&str> = m.items[1]
        .members
        .iter()
        .map(|m| m.signature.as_str())
        .collect();
    assert_eq!(
        members,
        [
            // A parameter property's decorator is the parameter's, shown on the constructor.
            "p: i64",
            "@dd(\"x\") x: i64 = 0",
            "@dd static readonly s: i64 = 1",
            "constructor(@dd public p: i64)",
            "@dd(\"m\") static m(@dd(\"p\") a: i64): i64",
            "@dd(\"o\") o(a: string): void",
        ]
    );
    let docs: Vec<&str> = m.items.iter().map(|i| i.doc.as_str()).collect();
    assert_eq!(docs[1..3], ["A class.", "A function."]);
    let member_docs: Vec<&str> = m.items[1].members.iter().map(|i| i.doc.as_str()).collect();
    assert_eq!((member_docs[1], member_docs[4]), ("A field.", "A method."));
}

/// std's intrinsics and host functions are documented as functions; other `declare`s are not.
#[test]
fn intrinsic_declarations_are_documented() {
    let src = "/** Swaps two elements. */\n@intrinsic\nexport declare function arraySwap<T>(xs: Mut<T[]>, i: usize, j: usize): void;\n\n/** Hidden. */\nexport declare function velt_rt_x(): void;\nexport declare host(\"env\") function now(): f64;\n";
    let m = extract("std/lang/intrinsics/array", src);
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["arraySwap", "now"]);
    assert_eq!(
        m.items[1].signature,
        "declare host(\"env\") function now(): f64"
    );
    let f = &m.items[0];
    assert_eq!(f.kind, Kind::Function);
    assert_eq!(
        f.signature,
        "@intrinsic declare function arraySwap<T>(xs: Mut<T[]>, i: usize, j: usize): void"
    );
    assert_eq!(f.doc, "Swaps two elements.");
}

/// A decorated `declare system` or `declare c("lib")` function is no intrinsic: it is left out
/// like an undecorated one, not documented as a `declare function` without its `system` / `c`.
#[test]
fn decorated_system_and_c_declarations_are_not_intrinsics() {
    let src = "declare decorator function mark(): FunctionDecorator;\n\n/** System. */\n@mark\nexport declare system function strlen(s: string): usize;\n\n/** Native. */\n@mark\nexport declare c(\"m\") function cos(x: f64): f64;\n\n/** Shown. */\n@mark\nexport declare function marked(n: i64): i64;\n";
    let m = extract("std/lang/intrinsics/probe", src);
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["marked"]);
    assert_eq!(
        m.items[0].signature,
        "@mark declare function marked(n: i64): i64"
    );
}

#[test]
fn plain_comments_and_template_text_are_not_docs() {
    let src = "// Module.\n\n// Not a doc.\nexport function f() {}\n\nconst t = `\n/// inside a template\n`;\nexport function g() {}\n";
    let m = extract("demo", src);
    assert_eq!(m.doc, "Module.");
    assert_eq!((m.items[0].doc.as_str(), m.items[1].doc.as_str()), ("", ""));
}

#[test]
fn plain_comments_above_documented_declarations() {
    let src = "// Module.

// Was a doc.
export function f() {}

/** Doc. */
export function g() {}

// Section.

export function h() {}

export class A {
  // Was a doc.
  x: i64;
  // Private.
  private y: i64;
}

// Not exported.
function k() {}
";
    let at = |needle: &str| src.find(needle).unwrap() as u32;
    assert_eq!(
        plain_comments_above_docs(src),
        [at("export function f"), at("x: i64")]
    );
}

#[test]
fn private_constructors_are_hidden() {
    let src = "export class A {\n  private constructor() {}\n}\n\nexport class B {\n  protected constructor(x: i64) {}\n}\n";
    let m = extract("demo", src);
    assert!(m.items[0].members.is_empty());
    let b: Vec<&str> = m.items[1]
        .members
        .iter()
        .map(|m| m.signature.as_str())
        .collect();
    assert_eq!(b, ["protected constructor(x: i64)"]);
}
