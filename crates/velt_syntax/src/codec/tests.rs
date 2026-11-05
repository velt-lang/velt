use std::path::{Path, PathBuf};

use velt_common::{FileId, Span};

use super::*;
use crate::{dump, parse_file, parse_ts_file};

fn parse(file: FileId, src: &str, ts: bool) -> ast::Module {
    if ts {
        parse_ts_file(file, src).0
    } else {
        parse_file(file, src).0
    }
}

/// Encoded in one file id and decoded in another, `src` comes out as a parse in the other.
fn round_trips(src: &str, ts: bool) {
    let bytes = encode_module(&parse(FileId(3), src, ts), FileId(3));
    let decoded = decode_module(&bytes, FileId(9)).expect("decodes");
    assert_eq!(dump(&decoded), dump(&parse(FileId(9), src, ts)));
}

const SAMPLE: &str = r#"
@a @ns.b("k", 1) export class Decorated<T> {
  @c field: T[] = [];
  constructor(@d private p: i64) {}
  @e() m(@f x: i64): i64 { return x; }
}
@g(1) enum DecoratedEnum { A }
export declare decorator function g(a: i64 = 3): Repeatable<TypeDecorator | FieldDecorator>;
import { a, type B as C } from "velt/x";
import * as ns from "./y";
import dflt, { other } from "./w";
import dflt2, * as ns2 from "./w2";
import type * as typesOnly from "./v";
export * from "./z";
export { q } from "./q";
// @ts-ignore
declare function ext(a: i64): void;
declare system function pread(fd: i32, n: usize): isize;
export declare host("m") function hostFn(s: string): HostRef | null;
export struct P { x: f64, y: f64; len(): f64 { return Math.sqrt(this.x ** 2 + this.y ** 2); } }
abstract class K<T extends A & B = D> extends Base<T> implements I {
  static #n = 1n;
  private readonly f?: string | null = null;
  protected constructor(public v: T) { super(); }
  get g(): number { return 1.5e3; }
  set g(x: number) {}
  *[Symbol.iterator](): Iterator<T> { yield* this.xs; }
}
interface I { m(): void; (x: number): string; new (s: string): I; readonly [k: string]: unknown }
enum E { A = 1, B, C = "c" }
type M<T> = { readonly [K in keyof T as `get${K & string}`]?: T[K] };
type Cond<T> = T extends (infer U)[] ? U : [T, ...T[], T?];
type F = <T>(x: T, ...r: T[]) => T throws Error;
type Ctor = abstract new () => object;
namespace N.Inner { export type Q = typeof ns.f; }
declare global { var g: number; }
declare module "velt/other" { interface X { y: 1 | -2 | "s" | true | undefined | void } }
extend<T> Array<T> { first(): T | null { return this.length > 0 ? this[0] : null; } }
function* gen(this: Foo, [a, b = 2]: [i32, i32], { c, d: { e } }: O, ...rest: string[]): Iterable<i32> {
  outer: for (let i = 0; i < 10; i++) { if (i % 2 == 0) continue outer; else break; }
  for await (const [k, v] of m) { await k; }
  for (const x of xs) { switch (x) { case 1: case 2: break; default: throw new Error(`e${x}!`); } }
  do { x >>>= 1; x ??= y; } while (!(x instanceof Y) && typeof x !== "string");
  try { f?.(a)?.b?.[c]!; } catch (e) { void delete r[k]; } finally { --x; x++; }
  m[a[i], b[j, k]] += n?.[i, j];
  const o = { a, b: 1, ...c, m() { return this; } };
  const p = Point { x: 1, y: 2 };
  let [h, , ...t] = xs as number[];
  using r = res();
  const big = 340282366920938463463374607431768211455u128;
  const nan = 0.1 + 2e-308;
  const arrow = async <T,>(x?: T, y: number = 2): Promise<T> => { return x!; };
  const fx = function* named() { yield; };
  const re = /a+b/gi;
  return 1;
}
const el = <div class="a" {...p} on:click={() => 1}><a.b />text{x}{...ys}<></></div>;
"#;

#[test]
fn every_kind_of_node_round_trips() {
    round_trips(SAMPLE, false);
    round_trips(
        "let x = <T>y;\nfunction f(a: string): A;\nfunction f(a: any) {}\n",
        true,
    );
    round_trips("", false);
    round_trips("export default @a class C { @b f = 1; }\n", false);
    round_trips("@a export default class {}\n", false);
    round_trips(
        "export default function f(a: string): string;\n@a export default function f(a: any) {}\n",
        false,
    );
}

/// What the `Import` literals of the desugarings say, and that the encoding keeps what parsed
/// ones say instead.
#[test]
fn an_import_keeps_how_it_was_written() {
    let flags = |src: &str| -> Vec<(bool, bool)> {
        let parsed = parse(FileId(3), src, false);
        let decoded = decode_module(&encode_module(&parsed, FileId(3)), FileId(3)).unwrap();
        let flags = |m: &ast::Module| -> Vec<(bool, bool)> {
            (m.items.iter())
                .filter_map(|item| match &item.kind {
                    ast::ItemKind::Import(i) => Some((i.default_import, i.type_only)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(flags(&parsed), flags(&decoded), "{src}");
        flags(&parsed)
    };
    assert_eq!(flags("import d from \"./a\";"), [(true, false)]);
    assert_eq!(flags("import type * as t from \"./a\";"), [(false, true)]);
    assert_eq!(flags("import { a } from \"./a\";"), [(false, false)]);
    // `export { C as default }`, after a decorated class on either side of `export`.
    assert_eq!(flags("export default @d class C {}"), [(false, false)]);
    assert_eq!(flags("@d export default class C {}"), [(false, false)]);
}

#[test]
fn spans_of_another_file_keep_their_file() {
    let mut m = parse(FileId(3), "const a = 1;", false);
    m.span.file = FileId(42);
    let bytes = encode_module(&m, FileId(3));
    let decoded = decode_module(&bytes, FileId(5)).unwrap();
    assert_eq!(decoded.span.file, FileId(42));
    assert_eq!(decoded.items[0].span.file, FileId(5));
}

/// `Span::DUMMY` is file 0's: a module encoded as file 0 still keeps it apart from its own
/// spans, so it does not become a real span of the file the module is decoded for.
#[test]
fn a_dummy_span_stays_dummy() {
    for file in [FileId(0), FileId(3)] {
        let mut m = parse(file, "const a = 1;", false);
        m.items[0].span = Span::DUMMY;
        let decoded = decode_module(&encode_module(&m, file), FileId(7)).unwrap();
        assert_eq!(decoded.items[0].span, Span::DUMMY);
        assert_eq!(decoded.span.file, FileId(7));
    }
}

#[test]
fn damaged_values_decode_to_none() {
    let bytes = encode_module(&parse(FileId(1), SAMPLE, false), FileId(1));
    // Every value cut short.
    for n in 0..bytes.len() {
        assert!(
            decode_module(&bytes[..n], FileId(1)).is_none(),
            "cut at {n}"
        );
    }
    // Trailing bytes, and another format.
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(decode_module(&longer, FileId(1)).is_none());
    let mut other = bytes.clone();
    other[7] ^= 1;
    assert!(decode_module(&other, FileId(1)).is_none());
    // Flipped bytes never panic (a different module or `None`).
    for i in (FORMAT.len()..bytes.len()).step_by(7) {
        let mut b = bytes.clone();
        b[i] ^= 0xa5;
        let _ = decode_module(&b, FileId(1));
    }
    // A length far beyond the value, and nesting deeper than any parse.
    let mut huge = FORMAT.to_vec();
    huge.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
    assert!(decode_module(&huge, FileId(1)).is_none());
    let too_deep = crate::with_parser_stack(|| {
        let mut m = parse(FileId(1), "const a = 1;", false);
        let ast::ItemKind::Var(v) = &mut m.items[0].kind else {
            panic!("a var")
        };
        let mut e = v.init.take().unwrap();
        for _ in 0..MAX_DEPTH {
            let (id, span) = (e.id, e.span);
            let kind = ast::ExprKind::Paren(Box::new(e));
            e = ast::Expr { id, kind, span };
        }
        v.init = Some(e);
        decode_module(&encode_module(&m, FileId(1)), FileId(1)).is_none()
    });
    assert!(too_deep);
}

fn std_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        if path.is_dir() {
            std_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|x| x == "velt" || x == "ts" || x == "tsx")
        {
            out.push(path);
        }
    }
}

/// Every file of the standard library decodes to the module a parse gives, spans included.
#[test]
fn every_std_file_round_trips() {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut files = vec![];
    std_files(&std, &mut files);
    assert!(files.len() > 100, "found {} std files", files.len());
    crate::with_parser_stack(|| {
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            let ts = path.extension().is_some_and(|x| x == "ts");
            let bytes = encode_module(&parse(FileId(3), &src, ts), FileId(3));
            let decoded = decode_module(&bytes, FileId(4));
            let decoded = decoded.unwrap_or_else(|| panic!("{} decodes", path.display()));
            let parsed = parse(FileId(4), &src, ts);
            assert!(dump(&decoded) == dump(&parsed), "{}", path.display());
        }
    });
}

/// `Index` gained `more` (the keys after the first) without growing `ExprKind`, so `a[i]`
/// costs what it did (#2179).
#[test]
fn an_index_with_several_keys_does_not_grow_expressions() {
    assert!(std::mem::size_of::<crate::ast::ExprKind>() <= 256);
}
