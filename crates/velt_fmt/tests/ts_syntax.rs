//! TypeScript syntax the AST keeps only in part (`let x!`, `declare` fields, `<const T>`, tuple
//! labels, `readonly` array and tuple types, method signatures, object-literal methods): the
//! formatter reproduces it as written. Each case is formatted source that must come back
//! unchanged (and keep its AST and comments, and be idempotent).

mod common;

use velt_fmt::format_source;

#[track_caller]
fn round_trips(src: &str) {
    let got = format_source(src).unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(got, src, "\n--- got ---\n{got}\n--- expected ---\n{src}");
    if let Err(msg) = common::check(src) {
        panic!("{msg}");
    }
}

#[test]
fn definite_assignment() {
    // Several declarators are printed as one declaration each.
    round_trips("let s!: string;\nlet a = 1;\nlet b!: number;\n");
    round_trips("class C {\n  m() {\n    let x!: number;\n    return x;\n  }\n}\n");
    round_trips("function f() {\n  let x!: number;\n  x = 1;\n}\n");
}

#[test]
fn declare_fields() {
    round_trips(
        "class B extends A {\n  declare cause: string;\n  declare readonly n: number;\n}\n",
    );
}

#[test]
fn const_type_parameters() {
    round_trips("function f<const T>(x: T): T {\n  return x;\n}\n");
    round_trips("function g<A, const T extends string[]>(a: A, x: T): T {\n  return x;\n}\n");
}

#[test]
fn tuple_labels() {
    round_trips("type P = [kind: string, b64: string];\n");
    round_trips("type Q = [string, count: number];\n");
}

#[test]
fn readonly_types() {
    round_trips("type R = readonly [string, number];\n");
    round_trips("function f(xs: readonly number[], ys: readonly (string | null)[]) {}\n");
    round_trips("type L = readonly [name: string, n: number];\n");
    round_trips("type M = (readonly number[])[] | (readonly [string])[];\n");
}

#[test]
fn method_signatures() {
    round_trips("interface I {\n  m?(x: string): number;\n  n?(): void;\n  p(a: number, b: string): boolean;\n}\n");
    round_trips("type O = { get(key: string): number; size?(): number };\n");
}

#[test]
fn object_literal_methods() {
    round_trips(
        "const o = {\n  get(obj, prop) {\n    return prop;\n  },\n  async load(n: number): Promise<number> {\n    return n;\n  },\n  plain: 1,\n};\n",
    );
    round_trips(
        "const h = {\n  id: (x: number): number => x,\n  twice(x: number) {\n    return 2 * x;\n  },\n};\n",
    );
}
