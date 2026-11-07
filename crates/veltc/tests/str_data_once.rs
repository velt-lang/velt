//! One inline-or-heap decision for a string's length and bytes (#2506, velt_vir
//! `lower/str_data.rs`): after `const n = strByteLen(s)` the pointer to the bytes is worked out
//! once, on the spot, when all that follows is quiet and reads them; everywhere else every read
//! works it out itself. The intrinsics are std-only, so a golden can't use them: std is copied
//! with a probe module added, and `velt` uses it through `$VELT_STD` (as inline_markers.rs does).
//!
//! `runs_the_same` is what matters in every case: a wrong memo reads a stale or unset pointer.
//! `VELT_RT_DEBUG_ALLOC=1` catches a read of a freed block.

use std::path::Path;
use std::process::{Command, Output};

mod no_window;
mod test_dir;

const PROBE: &str = r#"
import { noInline, strByteAt, strByteLen } from "velt/lang/intrinsics";

declare function velt_rt_strbuf_push_byte(out: string, byte: u8);
declare function velt_rt_strbuf_push_str(out: string, s: string);

const OFFSET: u64 = 7;

function twice(a: u8, b: u8): u64 {
  return (a as u64) + (b as u64);
}

export function fold(s: string): u64 {
  noInline();
  const n = strByteLen(s) as u64;
  let h = OFFSET;
  for (let i: u64 = 0; i < n; i++) {
    h = h * 31 + (strByteAt(s, i) as u64);
  }
  return h;
}

export function lengthOnly(s: string): u64 {
  noInline();
  const n = strByteLen(s) as u64;
  return n + 1;
}

export function inNestedBlock(s: string, c: bool): u8 {
  noInline();
  if (c) {
    const n = strByteLen(s) as u64;
    if (n > 0) {
      return strByteAt(s, 0);
    }
  }
  return 0;
}

export function afterTheBlock(s: string, c: bool): u8 {
  noInline();
  let m: u64 = 0;
  if (c) {
    const n = strByteLen(s) as u64;
    m = n + (strByteAt(s, 0) as u64);
  }
  return strByteAt(s, 1) + (m as u8);
}

export function underCondition(s: string, c: bool): u64 {
  noInline();
  const n: u64 = c ? (strByteLen(s) as u64) : 1;
  return twice(strByteAt(s, n - 1), strByteAt(s, n - 1));
}

export function underAnd(s: string, c: bool): u64 {
  noInline();
  const big = c && strByteLen(s) > 1;
  const at: u64 = big ? 1 : 0;
  return twice(strByteAt(s, at), strByteAt(s, at));
}

export function readAfterPush(out: string): u64 {
  noInline();
  const n = strByteLen(out) as u64;
  for (let i = 0; i < 64; i++) {
    velt_rt_strbuf_push_byte(out, 122);
  }
  return twice(strByteAt(out, n + 40), strByteAt(out, n + 40));
}

export function forwarded(out: string): void {
  noInline();
  velt_rt_strbuf_push_str(out, "wxyz");
}

export function readAfterCall(out: string): u64 {
  noInline();
  const n = strByteLen(out) as u64;
  forwarded(out);
  return twice(strByteAt(out, n), strByteAt(out, n));
}

export class Holder {
  s: string = "a".repeat(41);
}

export function borrowedConst(h: Holder): u64 {
  noInline();
  const t = h.s;
  const n = strByteLen(t) as u64;
  h.s = "b".repeat(50);
  return twice(strByteAt(t, n - 1), strByteAt(t, n - 1));
}

export function replacedByClosure(): u64 {
  noInline();
  let s = "a".repeat(41);
  const n = strByteLen(s) as u64;
  const change = () => {
    s = "b".repeat(50);
  };
  change();
  return twice(strByteAt(s, n - 1), strByteAt(s, n - 1));
}

export function ownerReplaced(s: string, f: () => void): u64 {
  noInline();
  const n = strByteLen(s) as u64;
  f();
  return twice(strByteAt(s, n - 1), strByteAt(s, n - 1));
}

export async function acrossAwait(s: string): Promise<u8> {
  const n = strByteLen(s) as u64;
  await Promise.resolve(0);
  return strByteAt(s, n - 1);
}

export async function afterAwait(s: string): Promise<u64> {
  await Promise.resolve(0);
  const n = strByteLen(s) as u64;
  return twice(strByteAt(s, n - 1), strByteAt(s, n - 1));
}

export function sink(b: u8): void {
  noInline();
  console.log(b);
}

export function inFinally(s0: string, c: bool): u8 {
  noInline();
  let s = s0 + "x".repeat(40);
  try {
    const n = strByteLen(s) as u64;
    if (c) {
      return strByteAt(s, n - 1);
    }
    return strByteAt(s, 0);
  } finally {
    s = "b".repeat(50);
    sink(strByteAt(s, 49));
  }
}

export function pushInFinally(out: string): u8 {
  noInline();
  try {
    const n = strByteLen(out) as u64;
    return strByteAt(out, n - 1);
  } finally {
    for (let i = 0; i < 64; i++) {
      velt_rt_strbuf_push_byte(out, 122);
    }
    sink(strByteAt(out, 40));
  }
}

export function nestedTwice(s: string, c: bool): u64 {
  noInline();
  if (c) {
    const n = strByteLen(s) as u64;
    return twice(strByteAt(s, n - 1), strByteAt(s, 0));
  }
  return 0;
}

export function assigned(s0: string): u64 {
  noInline();
  let s = s0 + "x".repeat(40);
  const n = strByteLen(s) as u64;
  s = "b".repeat(50);
  return twice(strByteAt(s, n - 1), strByteAt(s, 0));
}

export function generic<T>(s: string, x: T): u64 {
  noInline();
  const n = strByteLen(s) as u64;
  return twice(strByteAt(s, n - 1), strByteAt(s, 0));
}

export class Built {
  first: u64 = 1;
  constructor(s: string) {
    const n = strByteLen(s) as u64;
    this.first = twice(strByteAt(s, n - 1), strByteAt(s, 0));
  }
}

export function* acrossYield(s: string): Generator<u8> {
  const n = strByteLen(s) as u64;
  yield strByteAt(s, 0);
  yield strByteAt(s, n - 1);
}
"#;

const MAIN: &str = r#"
import {
  acrossAwait, acrossYield, afterAwait, afterTheBlock, assigned, Built, generic, inFinally, nestedTwice, pushInFinally, borrowedConst, fold, forwarded, Holder, inNestedBlock,
  lengthOnly, ownerReplaced, readAfterCall, readAfterPush, replacedByClosure, underAnd,
  underCondition,
} from "velt/zz_probe";

let owner = "c".repeat(41);

async function main() {
  const long = "abcdefgh".repeat(10);
  console.log(fold(""), fold("abc"), fold("héllo"), fold(long));
  console.log(lengthOnly("abc"), lengthOnly(long));
  console.log(inNestedBlock("abc", true), inNestedBlock(long, true), inNestedBlock("", true));
  console.log(afterTheBlock("abc", true), afterTheBlock(long, false));
  console.log(underCondition("abc", true), underCondition(long, false));
  console.log(underAnd("abc", true), underAnd(long, false));
  const builder = "abc".repeat(1);
  console.log(readAfterPush(builder));
  const second = "abc".repeat(1);
  console.log(readAfterCall(second));
  console.log(borrowedConst(new Holder()));
  console.log(replacedByClosure());
  console.log(
    ownerReplaced(owner, () => {
      owner = "d".repeat(4096);
    }),
  );
  console.log(await acrossAwait("abc"), await acrossAwait(long));
  console.log(await afterAwait("abc"), await afterAwait(long));
  console.log(inFinally("a", true), inFinally("a", false));
  const third = "abc".repeat(1);
  console.log(pushInFinally(third));
  console.log(nestedTwice("abc", true), nestedTwice(long, true), nestedTwice("abc", false));
  console.log(assigned("a"));
  console.log(generic("abc", 1), new Built(long).first);
  for (const b of acrossYield(long)) {
    console.log(b);
  }
}
"#;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

fn setup(dir: &test_dir::TestDir) -> Command {
    let std = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::create_dir_all(std.join("zz_probe")).unwrap();
    std::fs::write(std.join("zz_probe/index.velt"), PROBE).unwrap();
    std::fs::write(dir.path().join("main.velt"), MAIN).unwrap();
    let mut velt = no_window::command(env!("CARGO_BIN_EXE_velt"));
    velt.env("VELT_STD", &std)
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .current_dir(dir.path());
    velt
}

fn text(what: &str, o: &Output) -> String {
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{what}: {out}\n{err}");
    out
}

/// The blocks of each `name`'s function in the VIR, by the name in its symbol.
fn function<'a>(vir: &'a str, name: &str) -> &'a str {
    let symbol = format!("zz__probeN{}{name}", name.len());
    let at = vir
        .lines()
        .find(|l| l.starts_with("fn#") && l.contains(&symbol))
        .unwrap_or_else(|| panic!("no {name} in the VIR"));
    let from = vir.find(at).unwrap();
    let rest = &vir[from..];
    &rest[..rest.find("\n}").unwrap()]
}

/// How many times `f` works out whether a string is inline, the entry block's and the others'.
fn decisions(f: &str) -> (usize, usize) {
    let entry_end = f.find("\n  bb1:").unwrap_or(f.len());
    let count = |s: &str| s.matches(", 63_i64").count();
    (count(&f[..entry_end]), count(&f[entry_end..]))
}

fn total(f: &str) -> usize {
    let (entry, others) = decisions(f);
    entry + others
}

const EXPECTED: &str = "\
7 304891 9375185992 3754873402879105839
4 81
97 97 0
198 98
198 194
196 194
244
238
194
196
198
99 104
198 208
98
98
120 97
122
99
196 201 0
196
196 201
97
104
";

#[test]
fn the_memo_is_taken_only_where_it_holds() {
    let dir = test_dir::TestDir::new();
    let mut velt = setup(&dir);
    let vir = velt.args(["build", "--emit", "vir", "main.velt"]).output();
    let vir = text("vir", &vir.unwrap());

    // Length and pointer, both in the entry block, however often the loop reads.
    assert_eq!(decisions(function(&vir, "fold")), (2, 0));
    // Nothing reads the bytes: no pointer.
    assert_eq!(decisions(function(&vir, "lengthOnly")), (1, 0));

    // In a nested block too: one for the length, one for the pointer, however many reads.
    assert_eq!(total(function(&vir, "nestedTwice")), 2);
    // `s` assigned after the length: the rest isn't quiet. Two more are the `+`'s.
    assert_eq!(total(function(&vir, "assigned")), 5);
    // One read in the block and one past it: the read past it doesn't use the memo.
    assert_eq!(total(function(&vir, "afterTheBlock")), 3);
    // Each of these reads twice: once more than `fold`'s decisions, so no memo was taken.
    for name in [
        "underCondition",
        "underAnd",
        "readAfterPush",
        "readAfterCall",
        "borrowedConst",
        "replacedByClosure",
        "ownerReplaced",
    ] {
        assert_eq!(total(function(&vir, name)), 3, "{name}");
    }
    // Not in an async function, though nothing suspends after the length: this step leaves
    // them out (its poll function holds both reads and the length).
    // A generic instance, and a constructor.
    let of = |name: &str| {
        vir.split("\nfn#")
            .find(|f| f.contains(name) && f.contains(", 63_i64"))
            .map(total)
    };
    assert_eq!(of("zz__probeN7generic"), Some(3));
    assert_eq!(of("zz__probeN5Built"), Some(3));
    let poll = vir
        .split("\nfn#")
        .find(|f| f.contains("zz__probeN10afterAwait") && f.contains(", 63_i64"));
    assert_eq!(poll.map(total), Some(3));
}

#[test]
fn runs_the_same_in_debug_and_release() {
    let dir = test_dir::TestDir::new();
    let mut debug = setup(&dir);
    let debug = text("debug", &debug.args(["run", "main.velt"]).output().unwrap());
    assert_eq!(debug, EXPECTED);

    let dir = test_dir::TestDir::new();
    let mut release = setup(&dir);
    let args = ["run", "--release", "main.velt"];
    let release = text("release", &release.args(args).output().unwrap());
    assert_eq!(debug, release);
}
