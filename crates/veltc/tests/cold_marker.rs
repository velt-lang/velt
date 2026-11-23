//! `@cold` (std/lang/intrinsics/decorators.velt, re-exported by velt/compiler;
//! docs/internals/design/package-machine-code.md §1): a function whose calls are rare, though
//! they return. Lowering sets `vir::Function::rare`; `velt_opt` inlines such a function only where
//! it is trivial, LLVM gets `cold` (and `noinline` only with `@noInline`), and nothing treats a
//! call of it as one that does not return. Sema rejects it with `@inline`, on an async function or
//! a generator, on a `declare function` and on an abstract method.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

/// `velt <args> main.velt` with `main` as the program.
fn velt(dir: &Path, main: &str, args: &[&str]) -> Output {
    let file = dir.join("main.velt");
    std::fs::write(&file, main).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_BUILD_CACHE", "off")
        .current_dir(dir)
        .output()
        .unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A program whose `slow` (marked `MARK`) is called once, on a path the loop takes for one
/// element in a hundred: small enough that, called once, the inliner would take it in. Its
/// result is used after the call, so a call treated as one that does not return would lose it.
const PROGRAM: &str = r#"import { cold, noInline } from "velt/compiler";

MARK
function slow(x: i64): i64 {
  let y = x * 3 + 1;
  y = y * 5 + 2;
  y = y ^ (y >> 3);
  y = y * 7 + 3;
  return y % 1000;
}

function main() {
  let sum: i64 = 0;
  for (let i: i64 = 0; i < 1000; i++) {
    if (i % 100 === 7) {
      sum += slow(i);
    } else {
      sum += i;
    }
  }
  console.log(sum);
}
"#;

/// The header line of the VIR function whose symbol contains `name`, and the number of calls of
/// it, in `vir`.
fn vir_of<'a>(vir: &'a str, name: &str) -> (Option<&'a str>, usize) {
    let header = (vir.lines()).find(|l| l.starts_with("fn#") && l.contains(name));
    let calls = (vir.lines())
        .filter(|l| !l.starts_with("fn#") && l.contains(" call ") && l.contains(name))
        .count();
    (header, calls)
}

fn release(mark: &str, emit: &str) -> String {
    let dir = test_dir::TestDir::new();
    let main = PROGRAM.replace("MARK", mark);
    ok(&velt(
        dir.path(),
        &main,
        &["build", "--release", "--emit", emit],
    ))
}

#[test]
fn a_cold_function_is_rare_and_stays_a_call() {
    // Unmarked, the one call is inlined: the test needs that.
    let vir = release("", "vir");
    let (header, calls) = vir_of(&vir, "4slow");
    assert_eq!(calls, 0, "{vir}");
    assert!(header.is_none_or(|h| !h.contains(" rare")), "{vir}");
    // `@cold`: rare, and the call stays.
    let vir = release("@cold", "vir");
    let (header, calls) = vir_of(&vir, "4slow");
    let header = header.unwrap_or_else(|| panic!("no slow:\n{vir}"));
    assert!(header.contains(" rare"), "{header}");
    assert!(!header.contains(" cold"), "it returns: {header}");
    assert_eq!(calls, 1, "{vir}");
}

/// The LLVM attributes of `slow`'s definition: the `define` line, and the attribute group it
/// names, if any.
fn llvm_attrs(ir: &str) -> String {
    let define = (ir.lines())
        .find(|l| l.starts_with("define") && l.contains("4slow"))
        .unwrap_or_else(|| panic!("no define of slow:\n{ir}"));
    let mut out = define.to_string();
    for word in define.split_whitespace().filter(|w| w.starts_with('#')) {
        let prefix = format!("attributes {word} ");
        if let Some(group) = ir.lines().find(|l| l.starts_with(&prefix)) {
            out.push(' ');
            out.push_str(group);
        }
    }
    out
}

#[test]
fn llvm_compiles_a_cold_function_as_cold() {
    let attrs = llvm_attrs(&release("@cold", "llvm"));
    assert!(attrs.contains(" cold"), "{attrs}");
    assert!(!attrs.contains("noinline"), "{attrs}");
    let attrs = llvm_attrs(&release("@cold\n@noInline", "llvm"));
    assert!(
        attrs.contains(" cold") && attrs.contains("noinline"),
        "{attrs}"
    );
    let attrs = llvm_attrs(&release("@noInline", "llvm"));
    assert!(!attrs.contains(" cold"), "{attrs}");
}

#[test]
fn a_cold_function_returns_in_debug_and_release() {
    let dir = test_dir::TestDir::new();
    let main = PROGRAM.replace("MARK", "@cold");
    let debug = ok(&velt(dir.path(), &main, &["run"]));
    let release = ok(&velt(dir.path(), &main, &["run", "--release"]));
    let plain = ok(&velt(dir.path(), &PROGRAM.replace("MARK", ""), &["run"]));
    assert_eq!(debug, plain);
    assert_eq!(release, plain);
    assert_ne!(plain.trim(), "");
}

#[test]
fn the_cold_checks() {
    let main = r#"import { cold, inline, noInline } from "velt/compiler";

@cold
@inline
function both(x: i64): i64 {
  return x;
}

@cold
async function later(x: i64): Promise<i64> {
  return x;
}

@cold
function* gen(): Generator<i64> {
  yield 1;
}

abstract class Shape {
  @cold
  abstract area(): f64;
}

@cold
@noInline
function fine(x: i64): i64 {
  return x;
}

function main() {
  console.log(both(1) + fine(2));
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(dir.path(), main, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in [
        "3:1: error: a function is `@cold` or `@inline`, not both",
        "9:1: error: `@cold` doesn't apply to an async function or a generator",
        "14:1: error: `@cold` doesn't apply to an async function or a generator",
        "20:3: error: `@cold` applies to a function with a body, not to an abstract method",
    ] {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
    assert!(
        !err.contains("24:1:"),
        "`@cold @noInline` is allowed:\n{err}"
    );
}
