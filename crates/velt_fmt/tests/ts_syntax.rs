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

#[test]
fn symbol_keys() {
    round_trips("const KEY: unique symbol = Symbol(\"k\");\n");
    round_trips(
        "interface VNode {\n  readonly [KEY]: true;\n  [Symbol.iterator](): Iterator<number>;\n}\n",
    );
    round_trips("type T = { [KEY]?: number };\n");
    round_trips("function f(o: T): boolean {\n  return KEY in o && Symbol.iterator in o;\n}\n");
    round_trips("const o = { [KEY]: 1, a: 2 };\n");
}

#[test]
fn destructured_parameters_and_types_from_defaults() {
    round_trips(
        "function f([a, b = 1]: [string, number], { c, d: e }: P): string {
  return a;
}
",
    );
    round_trips(
        "function g(location: string, status = 302) {}
",
    );
    round_trips(
        "const h = xs.map(([k, v]) => k + v);
",
    );
    round_trips(
        "const r = (a: number, ...xs: number[]) => a;
",
    );
    round_trips(
        "class C {
  m({ x }: P = q) {}
}
",
    );
}

#[test]
fn rest_generic_and_construct_function_types() {
    round_trips(
        "type Emit = (event: string, ...args: number[]) => void;
",
    );
    round_trips(
        "type Make = <T>(description: string) => Token<T>;
",
    );
    round_trips(
        "type Ctor = new (buffer: ArrayBuffer) => View;
",
    );
    round_trips(
        "type O = { m(a: number, ...rest: string[]): void };
",
    );
}

#[test]
fn destructured_parameters_in_types() {
    round_trips(
        "type Obj = { m([a, b]: [number, number]): void };
",
    );
    round_trips(
        "type F = ([a, b]: [number, number]) => number;
",
    );
    round_trips(
        "interface I {
  m({ x, y: [z] }: P, n: number): void;
}
",
    );
    round_trips(
        "type G = (x: string, { a }: P) => void;
",
    );
}

#[test]
fn array_pattern_holes_stay_holes() {
    round_trips(
        "const f = ([, , c]: number[]) => c;
",
    );
    round_trips(
        "function g([, b]: [string, number], [_, d]: [string, number]) {}
",
    );
    round_trips(
        "function h() {
  let [, x, , y] = xs;
  const [a, , ] = ys;
  const [, ...rest] = zs;
  return x;
}
",
    );
}

#[test]
fn computed_keys() {
    round_trips("const r = { a: 1, [o.tag]: 2, [k + \"!\"]: 3 };\n");
    round_trips("function f(t: string) {\n  return { [t]: 1, ...rest };\n}\n");
}

#[test]
fn import_meta() {
    round_trips("const u = import.meta.url;\nconsole.log(import.meta.dirname);\n");
}

#[test]
fn declare_global_and_module_blocks() {
    round_trips("declare global {\n  // the hook\n  interface W {\n    a: number;\n  }\n}\n");
    round_trips(
        "declare module \"./m\" {\n  interface O {\n    b?: string;\n  }\n\n  export interface P {\n    c: number;\n  }\n}\n",
    );
}

#[test]
fn namespaces() {
    round_trips(
        "export namespace Define {
  // a prop
  export type Prop<T> = { value: T };

  interface Hidden {
    a: number;
  }
}
",
    );
    round_trips(
        "declare namespace A {
  namespace B {
    type X = number;
  }
}
",
    );
}

#[test]
fn bracketed_and_numeric_keys_keep_their_spelling() {
    // `[7]: 5` once printed as `7: 5`: brackets stay, and numeric names print as written.
    round_trips("const r = { a: 1, [7]: 5, [\"lit\"]: 3, 8: 4, 0x10: 6 };\n");
    round_trips("const s = { [\"q\"]: { [0]: 1 } };\n");
}

#[test]
fn members_without_semicolons_parse() {
    // A line break ends an interface or object type member, as in TypeScript.
    let src =
        "interface I {\n  a: number\n  m(): string\n}\ntype O = {\n  b: string\n  c?: number\n};\n";
    let got = format_source(src).unwrap_or_else(|d| panic!("refused: {d:?}"));
    round_trips(&got);
}

#[test]
fn call_and_construct_signatures_keep_their_form() {
    // Interfaces stay interfaces (they merge, and `extends` / `implements` name them), members
    // keep their order, and parameters their names, `?` and initializers.
    round_trips(
        "interface F {
  // c
  (x: number, y: string): string;
}
",
    );
    round_trips(
        "interface H {
  (x: number): string;
  stop(): void;
  n: number;
}
",
    );
    round_trips("interface E extends F {\n  (z: boolean): void;\n}\n");
    round_trips("interface P {\n  new (x: number): P;\n  <T>(y: T): T;\n}\n");
    round_trips("interface I {\n  (x: number): string;\n  [key: string]: number;\n}\n");
    round_trips("type Q = { (x?: number): void };\n");
    round_trips("type D = { (x = 1): void };\n");
    round_trips("type G = { <T>(x: T): T; z: string };\n");
    round_trips("type U = { new (x: number): Foo } | null;\n");
    round_trips("type S = { (): void; (x: number): number };\n");
    round_trips("let f: { (x: number): string } = g;\n");
}

#[test]
fn construct_and_generic_function_types_in_unions_keep_their_parentheses() {
    // `new (x: number) => Foo | null` would return `Foo | null`.
    round_trips("type U = (new (x: number) => Foo) | null;\n");
    round_trips("type A = (new () => Foo)[];\n");
    round_trips("type B = (<T>(x: T) => T) | null;\n");
    round_trips("type C = (new () => Foo) & { n: number };\n");
}

#[test]
fn call_signatures_are_formatted() {
    let got = format_source("interface F { (x:number):string; n:number }\ntype T = {(x:number):string}\n")
        .unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(
        got,
        "interface F {\n  (x: number): string;\n  n: number;\n}\n\ntype T = { (x: number): string };\n"
    );
    if let Err(msg) = common::check(&got) {
        panic!("{msg}");
    }
}

#[test]
fn a_type_alias_may_end_at_the_closing_brace() {
    let got = format_source("namespace N { type U = string }\ndeclare global {\n  namespace G { type V = number }\n}\n")
        .unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(
        got,
        "namespace N {\n  type U = string;\n}\n\ndeclare global {\n  namespace G {\n    type V = number;\n  }\n}\n"
    );
}
