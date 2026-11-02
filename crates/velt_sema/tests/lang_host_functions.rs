//! Host functions (`declare host("m") function`, https://veltlang.com/docs/reference/host-functions/): what crosses
//! to the JS host, the errors for what cannot, and how the buffers they write are borrowed.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, ExternFnDef, HostTy, IntTy, Program};

fn host_fn<'p>(p: &'p Program, name: &str) -> &'p ExternFnDef {
    p.defs
        .iter()
        .find_map(|d| match d {
            Def::ExternFn(x) if x.symbol == name => Some(x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no extern `{name}`"))
}

/// Every type that crosses, as a parameter and as the result, is recorded with the import
/// module.
#[test]
fn the_types_that_cross() {
    let p = ok_src(
        r#"import { HostRef } from "velt/host";

declare host("env") function nums(a: i8, b: u16, c: i32, d: u64, e: isize, f: f64, g: bool): f64;
declare host("env") function text(s: string, b: readonly u8[], w: u8[]): string;
declare host("env") function refs(r: HostRef, o: HostRef | null): HostRef | null;
declare host("env") function made(): HostRef;
declare host("env") function bytes(): u8[];
export declare host("other") function done(n: u32): void;

function main() {}
"#,
    );
    let h = host_fn(&p, "nums").host.as_ref().expect("a host function");
    assert_eq!(h.module, "env");
    assert_eq!(
        h.params,
        [
            HostTy::Int(IntTy::I8),
            HostTy::Int(IntTy::U16),
            HostTy::Int(IntTy::I32),
            HostTy::Int(IntTy::U64),
            HostTy::Int(IntTy::ISize),
            HostTy::F64,
            HostTy::Bool,
        ]
    );
    assert_eq!(h.ret, HostTy::F64);
    let h = host_fn(&p, "text").host.as_ref().unwrap();
    assert_eq!(
        h.params,
        [
            HostTy::Str,
            HostTy::Bytes { readonly: true },
            HostTy::Bytes { readonly: false }
        ]
    );
    assert_eq!(h.ret, HostTy::Str);
    let h = host_fn(&p, "refs").host.as_ref().unwrap();
    assert_eq!(
        h.params,
        [
            HostTy::Ref { nullable: false },
            HostTy::Ref { nullable: true }
        ]
    );
    assert_eq!(h.ret, HostTy::Ref { nullable: true });
    assert_eq!(
        host_fn(&p, "made").host.as_ref().unwrap().ret,
        HostTy::Ref { nullable: false }
    );
    assert_eq!(
        host_fn(&p, "bytes").host.as_ref().unwrap().ret,
        HostTy::Bytes { readonly: false }
    );
    let done = host_fn(&p, "done");
    assert_eq!(done.host.as_ref().unwrap().module, "other");
    assert_eq!(done.host.as_ref().unwrap().ret, HostTy::Void);
    assert!(done.system.is_none() && !done.is_async);
}

/// A parameter of another type is an error naming it and listing what crosses.
#[test]
fn a_parameter_that_cannot_cross_is_named() {
    let r = err_src(
        r#"declare host("env") function each(xs: i32[], cb: (x: i32) => void): void;
function main() {}
"#,
    );
    assert!(
        r.contains("parameter `xs` of host function `each` has type `i32[]`, which cannot cross to the host"),
        "{r}"
    );
    assert!(r.contains("parameter `cb` of host function `each`"), "{r}");
    assert!(
        r.contains(
            "a host function takes integers, `f64`, `bool`, `string`, `u8[]`, `readonly u8[]`"
        ),
        "{r}"
    );
    assert!(r.contains("`HostRef` and `HostRef | null`"), "{r}");
}

/// A result of another type, and a `Promise` result without `async`, are errors.
#[test]
fn results_and_async() {
    let r = err_src(
        r#"class Point {
  x: f64 = 0;
}
declare host("env") function point(): Point;
declare host("env") async function later(): Promise<Point>;
function main() {}
"#,
    );
    assert!(
        r.contains("host function `point` returns `Point`, which cannot cross to the host"),
        "{r}"
    );
    assert!(
        r.contains("host function `later` fulfills with `Point`, which cannot cross to the host"),
        "{r}"
    );
    assert!(
        r.contains("a host function returns `void`, integers"),
        "{r}"
    );
    // `HostRef | null | undefined` has two nothings: no pointer holds them.
    let r = err_src(
        r#"import { HostRef } from "velt/host";
declare host("env") function find(): HostRef | null | undefined;
function main() {}
"#,
    );
    assert!(r.contains("host function `find` returns"), "{r}");
    let r = err_src("declare host(\"env\") function later(): Promise<i32>;\nfunction main() {}\n");
    assert!(
        r.contains("host function `later` returns a `Promise` but is not `async`"),
        "{r}"
    );
    assert!(
        r.contains("declare host(\"env\") async function later(…)"),
        "{r}"
    );
    assert!(!r.contains("cannot cross"), "{r}");
    let r = err_src(
        "declare host(\"env\") async function twice(): Promise<Promise<i32>>;\nfunction main() {}\n",
    );
    assert!(
        r.contains("host function `twice` fulfills with `Promise<i32>`, which cannot cross"),
        "{r}"
    );
    let r = err_src(
        r#"import { HostRef } from "velt/host";
declare host("env") async function find(): Promise<HostRef | null | undefined>;
function main() {}
"#,
    );
    assert!(r.contains("host function `find` fulfills with"), "{r}");
    // The host could keep a live view of the bytes past the call.
    let r = err_src(
        "declare host(\"env\") async function fill(into: u8[]): Promise<void>;\nfunction main() {}\n",
    );
    assert!(
        r.contains("parameter `into` of async host function `fill` is a `u8[]` the host could write after the call"),
        "{r}"
    );
}

/// An async host function's promise settles with a type that crosses, and rejects with
/// `IoError`, which `await` throws.
#[test]
fn async_host_functions() {
    let p = ok_src(
        r#"import { HostRef } from "velt/host";
import { IoError } from "velt/io";

declare host("env") async function wait(ms: u32): Promise<void>;
declare host("env") async function load(key: string, data: readonly u8[]): Promise<string>;
declare host("env") async function open(): Promise<HostRef | null>;

async function main() {
  await wait(1);
  try {
    const text: string = await load("a", [0]);
    console.log(text);
  } catch (e) {
    const err: IoError = e;
    console.log(err.code, err.message);
  }
  const r = await open();
  console.log(r == null);
}
"#,
    );
    let wait = host_fn(&p, "wait");
    assert!(wait.is_async);
    assert_eq!(wait.host.as_ref().unwrap().ret, HostTy::Void);
    let load = host_fn(&p, "load").host.as_ref().unwrap();
    assert_eq!(load.params, [HostTy::Str, HostTy::Bytes { readonly: true }]);
    assert_eq!(load.ret, HostTy::Str);
    assert_eq!(
        host_fn(&p, "open").host.as_ref().unwrap().ret,
        HostTy::Ref { nullable: true }
    );
}

/// Type parameters, optional and rest parameters, the runtime's own import module and the glue's
/// `velt:` modules outside std are errors (two different declarations of one import: veltc's `native::tests`).
#[test]
fn declaration_errors() {
    let r = err_src(
        r#"declare host("env") function generic<T>(x: i32): void;
declare host("env") function opt(x?: i32): void;
declare host("env") function rest(...xs: u8[]): void;
declare host("velt") function write(x: i32): void;
declare host("") function nowhere(): void;
declare host("velt:worker") function accept(): u32;
function main() {}
"#,
    );
    assert!(r.contains("a host function has no type parameters"), "{r}");
    assert!(
        r.contains("host function `opt` cannot have an optional parameter"),
        "{r}"
    );
    assert!(
        r.contains("host function `rest` cannot have a rest parameter"),
        "{r}"
    );
    assert!(
        r.contains("the import module `velt` is the runtime's own"),
        "{r}"
    );
    assert!(r.contains("a host function needs an import module"), "{r}");
    assert!(
        r.contains("the import modules `velt:…` are the runtime glue's"),
        "{r}"
    );
}

/// A `u8[]` parameter is a buffer the host may write: its argument is a mutable borrow (a
/// read-only array is rejected), and a view of bytes is accepted, as for system functions.
#[test]
fn buffers_are_borrowed_as_written() {
    let p = ok_src(
        r#"declare host("env") function fill(b: u8[], s: string): void;
declare host("env") function send(b: readonly u8[]): void;

function main() {
  const b: u8[] = [1, 2, 3];
  fill(b, "x");
  fill(b.subarray(1, 2), "y");
  send(b);
  send(new Uint8Array(4));
}
"#,
    );
    assert!(host_fn(&p, "fill").host.is_some());
    let r = err_src(
        r#"declare host("env") function fill(b: u8[]): void;
const K: readonly u8[] = [1, 2];
function main() {
  fill(K);
}
"#,
    );
    assert!(r.contains("read-only"), "{r}");
}
