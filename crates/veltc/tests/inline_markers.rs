//! std's inlining decorators (std/lang/intrinsics/decorators.velt, std only, #2203): `@inline`
//! inlines every call of a function whatever its size, `@noInline` keeps every call a call, and
//! sema rejects `@inline` on a recursive function, both on one function, and either on a
//! generator, an async function or a `declare function`. They are std-only (a program's own
//! decorators of those names are data: tests/golden/lang/errors/decorators_known_by_role.velt),
//! so a golden file cannot use them: std is copied to a temp directory with a probe module added,
//! and `velt` uses it through `$VELT_STD` (as gated_functions.rs does).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

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

/// A std with the module `velt/zz_marked` (`probe`) added, and `main.velt` beside it; runs
/// `velt <args> main.velt`.
fn velt(dir: &test_dir::TestDir, probe: &str, args: &[&str]) -> Output {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
    }
    std::fs::create_dir_all(std.join("zz_marked")).unwrap();
    let probe = format!("import {{ inline, noInline }} from \"velt/lang/intrinsics\";\n{probe}");
    std::fs::write(std.join("zz_marked/index.velt"), probe).unwrap();
    let file = dir.path().join("main.velt");
    std::fs::write(&file, MAIN).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_STD", &std)
        .env("VELT_BUILD_CACHE", "off")
        .current_dir(dir.path())
        .output()
        .unwrap()
}

const MAIN: &str =
    "import { probe } from \"velt/zz_marked\";\n\nfunction main() {\n  console.log(probe(7));\n}\n";

/// `wide` (300 statements: past every size the inliner takes, even from a loop) marked `@inline`
/// and called three times; `tiny` (trivial, called once) marked `@noInline`.
const LINES: &str = "  y = y * 3 + 0;
  y = y * 4 + 1;
  y = y * 5 + 2;
  y = y * 6 + 3;
  y = y * 7 + 4;
  y = y * 3 + 5;
  y = y * 4 + 6;
  y = y * 5 + 7;
  y = y * 6 + 8;
  y = y * 7 + 9;
  y = y * 3 + 10;
  y = y * 4 + 11;
  y = y * 5 + 12;
  y = y * 6 + 13;
  y = y * 7 + 14;
  y = y * 3 + 15;
  y = y * 4 + 16;
  y = y * 5 + 17;
  y = y * 6 + 18;
  y = y * 7 + 19;
  y = y * 3 + 20;
  y = y * 4 + 21;
  y = y * 5 + 22;
  y = y * 6 + 23;
  y = y * 7 + 24;
  y = y * 3 + 25;
  y = y * 4 + 26;
  y = y * 5 + 27;
  y = y * 6 + 28;
  y = y * 7 + 29;
  y = y * 3 + 30;
  y = y * 4 + 31;
  y = y * 5 + 32;
  y = y * 6 + 33;
  y = y * 7 + 34;
  y = y * 3 + 35;
  y = y * 4 + 36;
  y = y * 5 + 37;
  y = y * 6 + 38;
  y = y * 7 + 39;
  y = y * 3 + 40;
  y = y * 4 + 41;
  y = y * 5 + 42;
  y = y * 6 + 43;
  y = y * 7 + 44;
  y = y * 3 + 45;
  y = y * 4 + 46;
  y = y * 5 + 47;
  y = y * 6 + 48;
  y = y * 7 + 49;
  y = y * 3 + 50;
  y = y * 4 + 51;
  y = y * 5 + 52;
  y = y * 6 + 53;
  y = y * 7 + 54;
  y = y * 3 + 55;
  y = y * 4 + 56;
  y = y * 5 + 57;
  y = y * 6 + 58;
  y = y * 7 + 59;
  y = y * 3 + 60;
  y = y * 4 + 61;
  y = y * 5 + 62;
  y = y * 6 + 63;
  y = y * 7 + 64;
  y = y * 3 + 65;
  y = y * 4 + 66;
  y = y * 5 + 67;
  y = y * 6 + 68;
  y = y * 7 + 69;
  y = y * 3 + 70;
  y = y * 4 + 71;
  y = y * 5 + 72;
  y = y * 6 + 73;
  y = y * 7 + 74;
  y = y * 3 + 75;
  y = y * 4 + 76;
  y = y * 5 + 77;
  y = y * 6 + 78;
  y = y * 7 + 79;
  y = y * 3 + 80;
  y = y * 4 + 81;
  y = y * 5 + 82;
  y = y * 6 + 83;
  y = y * 7 + 84;
  y = y * 3 + 85;
  y = y * 4 + 86;
  y = y * 5 + 87;
  y = y * 6 + 88;
  y = y * 7 + 89;
  y = y * 3 + 90;
  y = y * 4 + 91;
  y = y * 5 + 92;
  y = y * 6 + 93;
  y = y * 7 + 94;
  y = y * 3 + 95;
  y = y * 4 + 96;
  y = y * 5 + 97;
  y = y * 6 + 98;
  y = y * 7 + 99;
  y = y * 3 + 100;
  y = y * 4 + 101;
  y = y * 5 + 102;
  y = y * 6 + 103;
  y = y * 7 + 104;
  y = y * 3 + 105;
  y = y * 4 + 106;
  y = y * 5 + 107;
  y = y * 6 + 108;
  y = y * 7 + 109;
  y = y * 3 + 110;
  y = y * 4 + 111;
  y = y * 5 + 112;
  y = y * 6 + 113;
  y = y * 7 + 114;
  y = y * 3 + 115;
  y = y * 4 + 116;
  y = y * 5 + 117;
  y = y * 6 + 118;
  y = y * 7 + 119;
  y = y * 3 + 120;
  y = y * 4 + 121;
  y = y * 5 + 122;
  y = y * 6 + 123;
  y = y * 7 + 124;
  y = y * 3 + 125;
  y = y * 4 + 126;
  y = y * 5 + 127;
  y = y * 6 + 128;
  y = y * 7 + 129;
  y = y * 3 + 130;
  y = y * 4 + 131;
  y = y * 5 + 132;
  y = y * 6 + 133;
  y = y * 7 + 134;
  y = y * 3 + 135;
  y = y * 4 + 136;
  y = y * 5 + 137;
  y = y * 6 + 138;
  y = y * 7 + 139;
  y = y * 3 + 140;
  y = y * 4 + 141;
  y = y * 5 + 142;
  y = y * 6 + 143;
  y = y * 7 + 144;
  y = y * 3 + 145;
  y = y * 4 + 146;
  y = y * 5 + 147;
  y = y * 6 + 148;
  y = y * 7 + 149;
  y = y * 3 + 150;
  y = y * 4 + 151;
  y = y * 5 + 152;
  y = y * 6 + 153;
  y = y * 7 + 154;
  y = y * 3 + 155;
  y = y * 4 + 156;
  y = y * 5 + 157;
  y = y * 6 + 158;
  y = y * 7 + 159;
  y = y * 3 + 160;
  y = y * 4 + 161;
  y = y * 5 + 162;
  y = y * 6 + 163;
  y = y * 7 + 164;
  y = y * 3 + 165;
  y = y * 4 + 166;
  y = y * 5 + 167;
  y = y * 6 + 168;
  y = y * 7 + 169;
  y = y * 3 + 170;
  y = y * 4 + 171;
  y = y * 5 + 172;
  y = y * 6 + 173;
  y = y * 7 + 174;
  y = y * 3 + 175;
  y = y * 4 + 176;
  y = y * 5 + 177;
  y = y * 6 + 178;
  y = y * 7 + 179;
  y = y * 3 + 180;
  y = y * 4 + 181;
  y = y * 5 + 182;
  y = y * 6 + 183;
  y = y * 7 + 184;
  y = y * 3 + 185;
  y = y * 4 + 186;
  y = y * 5 + 187;
  y = y * 6 + 188;
  y = y * 7 + 189;
  y = y * 3 + 190;
  y = y * 4 + 191;
  y = y * 5 + 192;
  y = y * 6 + 193;
  y = y * 7 + 194;
  y = y * 3 + 195;
  y = y * 4 + 196;
  y = y * 5 + 197;
  y = y * 6 + 198;
  y = y * 7 + 199;
  y = y * 3 + 200;
  y = y * 4 + 201;
  y = y * 5 + 202;
  y = y * 6 + 203;
  y = y * 7 + 204;
  y = y * 3 + 205;
  y = y * 4 + 206;
  y = y * 5 + 207;
  y = y * 6 + 208;
  y = y * 7 + 209;
  y = y * 3 + 210;
  y = y * 4 + 211;
  y = y * 5 + 212;
  y = y * 6 + 213;
  y = y * 7 + 214;
  y = y * 3 + 215;
  y = y * 4 + 216;
  y = y * 5 + 217;
  y = y * 6 + 218;
  y = y * 7 + 219;
  y = y * 3 + 220;
  y = y * 4 + 221;
  y = y * 5 + 222;
  y = y * 6 + 223;
  y = y * 7 + 224;
  y = y * 3 + 225;
  y = y * 4 + 226;
  y = y * 5 + 227;
  y = y * 6 + 228;
  y = y * 7 + 229;
  y = y * 3 + 230;
  y = y * 4 + 231;
  y = y * 5 + 232;
  y = y * 6 + 233;
  y = y * 7 + 234;
  y = y * 3 + 235;
  y = y * 4 + 236;
  y = y * 5 + 237;
  y = y * 6 + 238;
  y = y * 7 + 239;
  y = y * 3 + 240;
  y = y * 4 + 241;
  y = y * 5 + 242;
  y = y * 6 + 243;
  y = y * 7 + 244;
  y = y * 3 + 245;
  y = y * 4 + 246;
  y = y * 5 + 247;
  y = y * 6 + 248;
  y = y * 7 + 249;
  y = y * 3 + 250;
  y = y * 4 + 251;
  y = y * 5 + 252;
  y = y * 6 + 253;
  y = y * 7 + 254;
  y = y * 3 + 255;
  y = y * 4 + 256;
  y = y * 5 + 257;
  y = y * 6 + 258;
  y = y * 7 + 259;
  y = y * 3 + 260;
  y = y * 4 + 261;
  y = y * 5 + 262;
  y = y * 6 + 263;
  y = y * 7 + 264;
  y = y * 3 + 265;
  y = y * 4 + 266;
  y = y * 5 + 267;
  y = y * 6 + 268;
  y = y * 7 + 269;
  y = y * 3 + 270;
  y = y * 4 + 271;
  y = y * 5 + 272;
  y = y * 6 + 273;
  y = y * 7 + 274;
  y = y * 3 + 275;
  y = y * 4 + 276;
  y = y * 5 + 277;
  y = y * 6 + 278;
  y = y * 7 + 279;
  y = y * 3 + 280;
  y = y * 4 + 281;
  y = y * 5 + 282;
  y = y * 6 + 283;
  y = y * 7 + 284;
  y = y * 3 + 285;
  y = y * 4 + 286;
  y = y * 5 + 287;
  y = y * 6 + 288;
  y = y * 7 + 289;
  y = y * 3 + 290;
  y = y * 4 + 291;
  y = y * 5 + 292;
  y = y * 6 + 293;
  y = y * 7 + 294;
  y = y * 3 + 295;
  y = y * 4 + 296;
  y = y * 5 + 297;
  y = y * 6 + 298;
  y = y * 7 + 299;";

const MARKED: &str = r#"
@inline
function wide(x: i64): i64 {
  let y = x;
LINES
  return y;
}

@noInline
function tiny(x: i64): i64 {
  return x + 1;
}

export function probe(a: i64): i64 {
  return wide(a) + wide(a + 1) + wide(a + 2) + tiny(a);
}
"#;

/// The VIR functions of `--emit vir`: each header line with the lines up to the next.
fn vir_functions(vir: &str) -> Vec<(&str, String)> {
    let mut out: Vec<(&str, String)> = vec![];
    for line in vir.lines() {
        if line.starts_with("fn#") {
            out.push((line, String::new()));
        } else if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// The release VIR of `probe`, and the number of calls of each function in it.
fn release_vir(probe: &str) -> (String, impl Fn(&str) -> usize) {
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, &["build", "--release", "--emit", "vir"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout).into_owned();
    let text = vir.clone();
    let calls = move |name: &str| {
        (text.lines())
            .filter(|l| !l.starts_with("fn#") && l.contains(" call ") && l.contains(name))
            .count()
    };
    (vir, calls)
}

#[test]
fn inline_inlines_every_call_and_no_inline_keeps_its_call() {
    let marked = MARKED.replace("LINES", LINES);
    // Without the decorators, `wide` stays a call and `tiny` is inlined: the test needs both.
    let (vir, calls) = release_vir(&marked.replace("@inline\n", "").replace("@noInline\n", ""));
    assert_eq!(calls("zz__markedN4wide"), 3, "{vir}");
    assert_eq!(calls("zz__markedN4tiny"), 0, "{vir}");
    let (vir, calls) = release_vir(&marked);
    let functions = vir_functions(&vir);
    assert_eq!(calls("zz__markedN4wide"), 0, "{vir}");
    assert_eq!(calls("zz__markedN4tiny"), 1, "{vir}");
    let tiny = (functions.iter())
        .find(|(h, _)| h.contains("zz__markedN4tiny"))
        .unwrap_or_else(|| panic!("no tiny:\n{vir}"));
    assert!(tiny.0.ends_with(" noinline {"), "{}", tiny.0);
}

#[test]
fn marked_functions_compute_the_same_in_debug_and_release() {
    let dir = test_dir::TestDir::new();
    let probe = MARKED.replace("LINES", LINES);
    let debug = velt(&dir, &probe, &["run"]);
    let release = velt(&dir, &probe, &["run", "--release"]);
    assert!(
        debug.status.success(),
        "{}",
        String::from_utf8_lossy(&debug.stderr)
    );
    assert!(
        release.status.success(),
        "{}",
        String::from_utf8_lossy(&release.stderr)
    );
    assert_eq!(debug.stdout, release.stdout);
    assert!(!debug.stdout.is_empty());
}

fn check_fails(probe: &str, messages: &[&str]) {
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    for m in messages {
        assert!(err.contains(m), "missing `{m}` in:\n{err}");
    }
}

#[test]
fn inline_on_a_recursive_function_is_an_error() {
    // Directly, and through another function: each marked function on the cycle is reported.
    let probe = r#"
@inline
function fact(n: i64): i64 {
  return n <= 1 ? 1 : n * fact(n - 1);
}

@inline
function even(n: i64): bool {
  return n == 0 ? true : odd(n - 1);
}

function odd(n: i64): bool {
  return n == 0 ? false : even(n - 1);
}

export function probe(a: i64): i64 {
  return fact(a) + (even(a) ? 1 : 0);
}
"#;
    check_fails(probe, &["`@inline` on a recursive function"]);
    let dir = test_dir::TestDir::new();
    let err = String::from_utf8_lossy(&velt(&dir, probe, &["check"]).stderr).into_owned();
    assert_eq!(
        err.matches("`@inline` on a recursive function").count(),
        2,
        "{err}"
    );
}

#[test]
fn a_function_calling_a_recursive_one_may_be_inlined() {
    // `outer` is not on the cycle, only calls into it: it may be marked.
    let probe = r#"
function fact(n: i64): i64 {
  return n <= 1 ? 1 : n * fact(n - 1);
}

@inline
function outer(n: i64): i64 {
  return fact(n) + 1;
}

export function probe(a: i64): i64 {
  return outer(a);
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, &["run", "--release"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5041");
}

#[test]
fn both_decorators_are_an_error() {
    // One field says how a function is inlined, so `@inline` and `@noInline` together are an
    // error at the second.
    let probe = r#"
@inline
@noInline
function both(x: i64): i64 {
  return x;
}

export function probe(a: i64): i64 {
  return both(a);
}
"#;
    check_fails(
        probe,
        &["4:1: error: a function is `@inline` or `@noInline`, not both"],
    );
}

#[test]
fn the_decorators_apply_to_plain_functions_and_methods() {
    // On a generator or an async function (whose body is compiled into a resume function the
    // decorator would not reach) and on a `declare function` (no body): each is an error.
    let probe = r#"
@inline
function* gen(n: i64): Generator<i64> {
  yield n;
}

@noInline
async function later(n: i64): Promise<i64> {
  return n;
}

class Box {
  @inline
  async load(n: i64): Promise<i64> {
    return n;
  }
}

@noInline
declare function velt_rt_zz_probe(n: i64): i64;

export function probe(a: i64): i64 {
  let s = a;
  for (const v of gen(a)) {
    s += v;
  }
  later(a);
  new Box().load(a);
  return s;
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, &["check"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    let resumable = "doesn't apply to an async function or a generator";
    assert_eq!(err.matches(resumable).count(), 3, "{err}");
    assert!(err.contains("`@inline` doesn't apply"), "{err}");
    assert!(err.contains("`@noInline` doesn't apply"), "{err}");
    assert!(
        err.contains("`@noInline` applies to a function with a body, not to a `declare function`"),
        "{err}"
    );
}

#[test]
fn the_decorators_on_methods_are_accepted() {
    let probe = r#"
class Counter {
  n: i64 = 0;

  @inline
  bump(by: i64): i64 {
    this.n += by;
    return this.n;
  }

  @noInline
  static twice(x: i64): i64 {
    return x * 2;
  }
}

export function probe(a: i64): i64 {
  const c = new Counter();
  return c.bump(a) + Counter.twice(a);
}
"#;
    let dir = test_dir::TestDir::new();
    let out = velt(&dir, probe, &["run", "--release"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "21");
}
