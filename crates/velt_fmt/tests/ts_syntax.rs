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
fn var_declarations_keep_var() {
    round_trips("function f(n: number) {\n  var hi = n / 16;\n  var lo = n % 16;\n  let s = 1;\n  return hi + lo + s;\n}\n");
    let got = format_source(
        "function f() {\n  var a = 1, b = 2;\n  let c = 3, d = 4;\n  return a + b + c + d;\n}\n",
    )
    .unwrap();
    assert_eq!(got, "function f() {\n  var a = 1;\n  var b = 2;\n  let c = 3;\n  let d = 4;\n  return a + b + c + d;\n}\n");
}

#[test]
fn async_generic_object_literal_methods() {
    round_trips(
        "const o = {
  async go<T>(x: T): Promise<T> {
    return x;
  },
};
console.log(o);
",
    );
}

#[test]
fn let_after_var_stays_let() {
    // A `for (let …)` after a `var` statement (its declaration starts at the name).
    round_trips(
        "function f(): void {\n  var x = 1;\n  for (let i = 0; i < 1; i++) {}\n  for (let i = 0, j = 1; i < j; i++) {}\n  console.log(x);\n}\n",
    );
    let got =
        format_source("function f(): void {\n  var x = 1;\n  let y = 2, z = 3;\n}\n").unwrap();
    assert_eq!(
        got,
        "function f(): void {\n  var x = 1;\n  let y = 2;\n  let z = 3;\n}\n"
    );
}

#[test]
fn var_keeps_var_across_nested_declarations() {
    // The first initializer contains declarations; the next declarator is still a `var`.
    let got = format_source(
        "function f() {\n  var a = () => { let x = 1; return x; }, b = 2;\n  var c = { m() { const q = 1; return q; } }, d = 3;\n  let e = () => { var y = 1; return y; }, g = 4;\n  return b + d + g;\n}\n",
    )
    .unwrap();
    assert_eq!(
        got,
        "function f() {\n  var a = () => {\n    let x = 1;\n    return x;\n  };\n  var b = 2;\n  var c = {\n    m() {\n      const q = 1;\n      return q;\n    },\n  };\n  var d = 3;\n  let e = () => {\n    var y = 1;\n    return y;\n  };\n  let g = 4;\n  return b + d + g;\n}\n"
    );
    round_trips(&got);
}

#[test]
fn optional_parameters_in_function_types() {
    round_trips("type F = (app: App, options?: Options) => void;\n");
    round_trips("type F = (x?: string | null) => string;\n");
}

#[test]
fn optional_and_rest_tuple_elements() {
    round_trips("type A = [x?: number, ...rest: string[]];\n");
    round_trips("type B = [string, number?];\n");
    round_trips("type C = readonly [Ref, ...A];\n");
}

#[test]
fn generic_method_signatures_in_object_types() {
    round_trips("type S = { run<T>(fn: () => T): T | undefined; stop(): void };\n");
    round_trips("interface I {\n  wrap?<A>(a: A): A;\n  name: string;\n}\n");
}

#[test]
fn catch_binding_annotations() {
    round_trips("try {\n  f();\n} catch (e: unknown) {\n  g(e);\n}\n");
    round_trips("try {\n  f();\n} catch (err: any) {\n  g(err);\n}\n");
}
