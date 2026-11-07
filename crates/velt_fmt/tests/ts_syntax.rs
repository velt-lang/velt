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
fn object_literal_accessors() {
    round_trips(
        "const o = {\n  get size(): number {\n    return 1;\n  },\n  set size(v: number) {\n    log(v);\n  },\n  get(key) {\n    return key;\n  },\n};\n",
    );
}

#[test]
fn object_literal_methods_using_this() {
    // The parser gives these a leading `this` parameter, which is not written.
    round_trips(
        "const o = {
  n: 1,
  inc(by: number) {
    this.n += by;
  },
  get double() {
    return this.n * 2;
  },
  set double(v) {
    this.n = v / 2;
  },
  *each(): Generator<number> {
    yield this.n;
  },
  f: function () {
    return this.n;
  },
};
",
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
    let got = format_source(
        "interface F { (x:number):string; n:number }\ntype T = {(x:number):string}\n",
    )
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
    let got = format_source(
        "namespace N { type U = string }\ndeclare global {\n  namespace G { type V = number }\n}\n",
    )
    .unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(
        got,
        "namespace N {\n  type U = string;\n}\n\ndeclare global {\n  namespace G {\n    type V = number;\n  }\n}\n"
    );
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

#[test]
fn literal_and_number_member_names_keep_their_form() {
    round_trips(
        "interface I {\n  [\"x-y\"]: number;\n  [2]: string;\n  1.5: string;\n  [-1]: string;\n  [Flags.SKIP]?: boolean;\n}\n",
    );
    round_trips("class C {\n  [\"size\"] = 3;\n  1.5 = \"a\";\n  [-2] = \"m\";\n}\n");
    round_trips("type O = { 1.5: string; [\"a b\"]: number };\n");
    round_trips(
        "const o = { 1.50: \"y\", [-1]: \"n\", [\"p\"]: 2 };\nconst { 1.5: a, [-1]: b } = o;\n",
    );
}

#[test]
fn static_blocks() {
    round_trips(
        "class S {\n  static n: number = 1;\n  static {\n    S.n += 1;\n  }\n  static {}\n}\n",
    );
}

#[test]
fn accessors_in_object_types() {
    round_trips("type T = {\n  get v(): number;\n  set v(x: number);\n  label: string;\n};\n");
    round_trips("type G<X> = {\n  get item(): X;\n};\n\ninterface I {\n  get v(): number;\n}\n");
    round_trips("function f(x: { get size(): number }): number {\n  return x.size;\n}\n");
    round_trips("let a: { get n(): number; set n(v: number) };\nlet b: { set m(x: string) };\n");
    // Each accessor is kept as written: the setter first, a setter of its own type.
    round_trips("let y: { set b(val: number); get b(): number };\n");
    round_trips("let z: { get w(): string; set w(input: string | number) };\n");
    // Members named `get` and `set` stay fields and methods.
    round_trips("let c: { get: () => number; set(x: number): void };\n");
}

#[test]
fn as_as_a_name() {
    round_trips("function as(x: any): any {\n  return x;\n}\n\nconst v = as(as(1)) as number;\n");
}

#[test]
fn type_only_namespace_import() {
    // Without `type` it would evaluate the module.
    round_trips("import type * as ns from \"./a\";\n");
    round_trips("import * as ns from \"./a\";\n");
}

#[test]
fn default_function_overloads() {
    // Each signature says `export default`; without it the next parse refuses them (TS2383).
    round_trips(
        "export default function f(a: string): string;\nexport default function f(a: number): number;\nexport default function f(a: any): any {\n  return a;\n}\n",
    );
    round_trips(
        "export default function (a: string): string;\nexport default function (a: any): any {\n  return a;\n}\n",
    );
    round_trips(
        "export default async function f(a: string): Promise<string>;\nexport default async function f(a: any): Promise<any> {\n  return a;\n}\n",
    );
}

#[test]
fn default_expressions_print_as_written() {
    round_trips("export default NaN;\n");
    round_trips("export default x;\nconst x = 1;\n");
    round_trips("export * as default from \"./a\";\n");
    round_trips(
        "export default interface Foo {\n  a: number;\n}\n\nexport default function bar() {}\n",
    );
}

#[test]
fn decorators() {
    round_trips("declare decorator function name(key: string): FieldDecorator;\n");
    round_trips("@a\n@ns.b(\"k\", 1)\nexport function f(@c x: number, @d() y: number) {}\n");
    round_trips("@a\nexport class C {\n  @b\n  x = 1;\n\n  @c(1)\n  @d\n  m() {}\n\n  constructor(@e private readonly p: number) {}\n}\n");
    round_trips("@(a.b)\nenum E {\n  A,\n}\n");
    round_trips("// about f\n@a\nfunction f() {}\n");
}

#[test]
fn decorators_go_on_their_own_lines_before_export() {
    let got = format_source("export @a class C { @b x = 1; }").unwrap();
    assert_eq!(got, "@a\nexport class C {\n  @b\n  x = 1;\n}\n");
}

/// Formats `src`, and the result again: both must parse and agree.
fn formats_stably(src: &str) -> String {
    let once = format_source(src).unwrap();
    let twice = format_source(&once).unwrap_or_else(|e| panic!("{once}\ndoesn't parse: {e:?}"));
    assert_eq!(once, twice, "not stable for {src}");
    once
}

#[test]
fn a_comment_after_a_parameter_decorator_stays_after_it() {
    let got = formats_stably("function f(@c // why\n  d: i64) {}\n");
    assert!(got.contains("  @c // why\n  d: i64,\n"), "{got}");
    let got = formats_stably("function f(@c /* why */ d: i64) {}\n");
    assert_eq!(got, "function f(@c /* why */ d: i64) {}\n");
    let got = formats_stably("class C { constructor(@c // why\n  private readonly d: i64) {} }\n");
    assert!(
        got.contains("@c // why\n    private readonly d: i64"),
        "{got}"
    );
    assert_eq!(got.matches("// why").count(), 1, "{got}");
    // Between the decorators of two parameters, and before the second's decorator.
    let src = "function f(@a /* KEEP1 */ b: i64, /* KEEP2 */ @a c: i64) {}\n";
    assert_eq!(formats_stably(src), src);
    let src = "function f(@a /* one */ @b /* two */ c: i64) {}\n";
    assert_eq!(formats_stably(src), src);
}

/// A comment after a declaration's decorator stays between the decorators or before the
/// declaration: never inside a class body or a parameter list, where a doc comment would stop
/// documenting the class.
#[test]
fn a_comment_after_a_decorator_stays_before_the_declaration() {
    for src in [
        "@a\n// c\nclass C {}\n",
        "@a\n/** doc */\nclass C {}\n",
        "@a // c\nclass C {}\n",
        "@a /* x */\n@a(1)\nclass C {}\n",
        "@a\n// between\n@b\nfunction f() {}\n",
        "@a /* y */\nexport class F {}\n",
        "@a /* e */\nenum E {\n  A,\n}\n",
        "class E {\n  @a /* c */\n  n() {}\n  @a // c\n  o() {}\n  @a\n  /** doc */\n  x: i64 = 1;\n}\n",
    ] {
        assert_eq!(formats_stably(src), src);
    }
    assert_eq!(
        formats_stably("@a /* x */ @a(1) class C {}\n"),
        "@a /* x */\n@a(1)\nclass C {}\n"
    );
    assert_eq!(
        formats_stably("class E { @a /* c */ n() {} }\n"),
        "class E {\n  @a /* c */\n  n() {}\n}\n"
    );
    assert_eq!(
        formats_stably("class E { @a /* x */ @a(1) p() {} }\n"),
        "class E {\n  @a /* x */\n  @a(1)\n  p() {}\n}\n"
    );
}

#[test]
fn a_method_decorator_stays_on_the_implementation() {
    let want =
        "class D {\n  m(x: number): void;\n  m(x: string): void;\n  @logged\n  m(x: any) {}\n}\n";
    round_trips(want);
    let on_one_line =
        "class D {\n  m(x: number): void;\n  m(x: string): void;\n  @logged m(x: any) {}\n}\n";
    assert_eq!(formats_stably(on_one_line), want);
    // Velt reads it from the first signature too, and TypeScript refuses it there (TS1249).
    let on_the_first =
        "class D {\n  @logged m(x: number): void;\n  m(x: string): void;\n  m(x: any) {}\n}\n";
    assert_eq!(formats_stably(on_the_first), want);
    // Modifiers stay after the decorator, and comments where they were.
    round_trips("class D {\n  // a\n  static m(x: number): void;\n  // b\n  @logged\n  static m(x: any) {}\n}\n");
    round_trips("abstract class D {\n  @logged\n  abstract m(x: number): void;\n}\n");
    // A decorated abstract property keeps its setter: it is not `readonly`.
    round_trips("abstract class D {\n  @f\n  abstract x: number;\n  @f\n  abstract readonly y: string;\n}\n");
}

#[test]
fn a_decorator_keeps_its_type_arguments() {
    round_trips("@fac<string>(\"a\")\nclass C {\n  @ns.fac<Array<i64>>()\n  m() {}\n}\n");
}

#[test]
fn decorators_on_a_default_export() {
    let class = "@a\nexport default class C {\n  @b\n  m() {}\n}\n";
    round_trips(class);
    assert_eq!(
        formats_stably("export default @a class C { @b m() {} }"),
        class
    );
    assert_eq!(
        formats_stably("@a export default class C { @b m() {} }"),
        class
    );
    for want in [
        "@a\nexport default class {}\n",
        "@a(1)\n@b\nexport default class extends B {}\n",
        "@a\nexport default abstract class C {}\n",
        "@a\nexport default struct S {}\n",
        "@a\nexport default function f() {}\n",
        "@a\nexport default function () {}\n",
        "@a\nexport default async function f() {}\n",
    ] {
        round_trips(want);
    }
    assert_eq!(
        formats_stably("export default @a class {}"),
        "@a\nexport default class {}\n"
    );
    // A comment after the decorator is no reason to lose `export default`.
    assert!(formats_stably("@a /* why */ export default class C {}\n")
        .contains("export default class C {"));
    assert!(formats_stably("@a // export\nclass C {}\n").starts_with("@a"));
    // Without `export default`, a comment that says it changes nothing.
    assert!(!formats_stably("@a /* export */ class C {}\n").contains("export default"));
}

#[test]
fn a_functions_decorators_stay_on_the_implementation() {
    let named = "export default function f(a: string): string;\n@d\nexport default function f(a: any): any {\n  return a;\n}\n";
    round_trips(named);
    for written in [
        "@d export default function f(a: string): string;\nexport default function f(a: any): any {\n  return a;\n}\n",
        "export default @d function f(a: string): string;\nexport default function f(a: any): any {\n  return a;\n}\n",
        "export default function f(a: string): string;\nexport default @d function f(a: any): any {\n  return a;\n}\n",
    ] {
        assert_eq!(formats_stably(written), named);
    }
    round_trips("export default function (a: string): string;\n@d\nexport default function (a: any): any {}\n");
    round_trips("export default async function f(a: string): Promise<string>;\n@d\nexport default async function f(a: any): Promise<any> {}\n");
    // Not a default: the same place, and comments stay where they were.
    round_trips("function f(a: string): string;\n@d\nfunction f(a: any): any {}\n");
    round_trips("export function f(a: string): string;\n@d\nexport function f(a: any): any {}\n");
    round_trips("// a\nfunction f(a: string): string;\n// b\n@d\nfunction f(a: any): any {}\n");
    assert_eq!(
        formats_stably(
            "@d\nexport function f(a: string): string;\nexport function f(a: any): any {}\n"
        ),
        "export function f(a: string): string;\n@d\nexport function f(a: any): any {}\n"
    );
    // One without overloads keeps them before `export`.
    round_trips("@d\nexport default function f(a: any): any {}\n");
}
