//! Unusual but valid syntax and comment placements: formatting must keep the AST and every
//! comment, and be idempotent. (Exact layouts are covered by `snapshots.rs`.)

mod common;

#[track_caller]
fn holds(src: &str) {
    assert!(common::parses(src), "test input does not parse:\n{src}");
    if let Err(msg) = common::check(src) {
        panic!("{msg}");
    }
}

#[test]
fn expressions() {
    holds(
        "function f() {
  const a = new Foo; const b = new ns.Bar<T>;
  const o = { \"a-b\": 1, 'c': 2, d, ...e };
  const s = P {}; const t = P { x: 1, ...q };
  const g = identity<string>(\"x\"); const h = f?.(x); const i = o?.[k]; const j = o?.p;
  const u = !(a) || ~x || -(-x) || + +x || - --x || - -x;
  const w = [...xs, 1]; call(...args);
  const c = a ? b : c ? d : e;
  const z = await load(); const y = x as (A | B); const v = x as i64 | null;
  const th = (x: i64): i64 throws E => x; const ft: () => Promise<i64> throws E = th2;
  const arrow = async (x) => x; const arrow2 = async () => {}; const arrow3 = (acc: i64) => acc;
  const obj = () => ({ a: 1 });
  x >>= 1; y >>>= 2; z **= 3; q ??= 4; r ||= s; m &&= n;
  const shifted = a >> b >>> c << d;
  const cmp = a >= b && c <= d && e > f && g < h;
  super.method(1); this.x.y().z;
  const nested = [[1, 2], [3, 4]];
  const tpl = `${a}${`${b}`}`;
}",
    );
}

#[test]
fn resource_management() {
    holds(
        "class R { [Symbol.dispose]() {} async   [Symbol.asyncDispose]() {} }
interface D { [Symbol.dispose](): void; }
async function f(r: R) {
  using a = open();   await using b: R = await connect();
  r[Symbol.dispose](); r . x[Symbol.dispose]();
}",
    );
}

#[test]
fn statements() {
    holds(
        "function f() {
  ;
  outer: { break outer; }
  do x++; while (c);
  for (x = 0; ; ) { break; }
  for (; i < n; ) i++;
  for (const [k, v] of m) {}
  for (let { a, b: c } of xs) {}
  try { a(); } catch { b(); }
  try { a(); } finally { b(); }
  if (a) {} else if (b) {} else {}
  function inner(): void {}
  struct Local { x: i64 }
  throw new Error(\"x\");
}",
    );
}

#[test]
fn items_and_types() {
    holds(
        "import \"side-effect\";
import { a as b, } from 'x';
export const X: [i64, string] = [1, \"a\"];
export declare async function ext(a: i64): i64;
export type F = (x: i64) => (y: i64) => Map<string, (z: i64) => void>;
type G<T> = T[][] | ((A | B)[]) | null;
class Empty {}
struct S<T extends A & (B | C)> implements I { readonly a?: T; b: i64 = 1, }
interface I<T> extends A, B { f: T; g<U>(x: U): T; async h(); }
enum E { A, B = 2, }
enum Dir { Up = \"UP\", Down = 'DOWN' }
type Lit = \"a\" | 'b' | -1 | 2.5 | true | 1u8;
type Obj = { kind: \"a\"; x: i64, y: { z: string } } | {};
extend Foo { static make(): Foo { return new Foo(); } }
class P { private a: i64; static readonly PI: f64 = 3.14; private static h(): i64 { return 1; } get n(): i64 { return 1; } set n(v: i64) { this.a = v; } set(k: i64) {} }
interface Q { get area(): f64; get x(): i64 { return 1; } set area(v: f64); }",
    );
}

#[test]
fn patterns() {
    holds(
        "function f() {
  const [a, , b, ...rest] = xs;
  const [c, ,] = ys;
  const { d, e: [g, h], ...others } = o;
  switch (v) {
    case 1: case 2:
      a();
    case -5: { b(); break; }
    case 'x': default:
      c(); break;
    case A.B:
  }
  switch (w) {}
}",
    );
}

#[test]
fn comments_in_odd_places() {
    holds(
        "/* a */ function /* b */ f /* c */ (/* d */ x /* e */: /* f */ i64 /* g */) /* h */ : i64 /* i */ {
  let /* j */ y: /* k */ i64 = /* l */ 1; // m
  const z = `${ /* inside template */ x }`;
  return /* n */ y
    // o
    + x;
}
const e = { /* empty */ };
const l = [ // lonely
];
enum E {
  A, // first
  // before B
  B,
  // end of enum
}
class C {
  x: i64; /* after x */ y: i64;
  // end of class
}
function h() {
  switch (v) { // after head
    case 1: // one
      a();
    // before default
    default:
      b(); // after b
    // end of switch
  }
}
function g() {
  if (a) {
    b();
  } // after then
  else {
    c();
  }
  call(a, /* mid */ b, c /* end */);
  obj.method(x) // trailing chain
    .other(y);
}
/* last */",
    );
}

#[test]
fn crlf_and_bom() {
    holds("\u{feff}// bom\r\nfunction f() {\r\n  const t = `a\r\nb`;\r\n}\r\n");
}

/// Comma lists in a `for` head are desugared by the parser and printed back as written.
#[test]
fn for_comma_lists() {
    let src = "function f() {
  l: for (let i = 0, j: usize = 3; i < j; i++, j--) {
    continue l;
  }
  for (a = 1, b = 2; a < b; a++, b += 0, c()) {}
}
";
    holds(src);
    assert_eq!(velt_fmt::format_source(src).ok().as_deref(), Some(src));
}

/// `bool` and `boolean` name the same type; the formatter keeps the spelling written (#353).
#[test]
fn bool_and_boolean_keep_their_spelling() {
    let src = "function f(a: bool, b: boolean): bool | boolean {
  const xs: Array<boolean> = [a];
  const g: (x: bool) => boolean = (x) => !x;
  return g(b) && xs[0];
}
";
    holds(src);
    assert_eq!(velt_fmt::format_source(src).ok().as_deref(), Some(src));
}

#[test]
fn readonly_fields_in_object_types() {
    holds("type User = { readonly id: number; readonly email?: string; readonly: bool };\n");
    let out = velt_fmt::format_source("type U = {readonly   id : i64};\n").unwrap();
    assert_eq!(out, "type U = { readonly id: i64 };\n");
}
