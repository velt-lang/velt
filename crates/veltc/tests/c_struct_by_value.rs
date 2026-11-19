//! C structs passed to and returned from C functions by value (#2187 layer 3,
//! docs/internals/design/c-ffi.md "C structs"): a differential test of the target's C calling
//! convention. A package's C `sources`, compiled by zig's clang, define for each struct shape a
//! function that builds one from a seed, one that checks every field of one it is given, and one
//! that returns what it is given; Velt builds the same values, so both directions are compared
//! field by field, in debug (Cranelift) and release (LLVM) builds. Further functions take structs
//! after the registers run out (a struct takes all its registers or goes on the stack whole), and
//! return one in memory with arguments around it. These tests need `VELT_ZIG` as
//! tests/c_sources.rs's do, and run on x86-64 Linux and macOS, whose System V convention
//! lowering has.

#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

/// A field type: its C spelling, its Velt type, and how a value is made from `base` (an `i64`)
/// in C and in Velt.
struct Field {
    c: &'static str,
    velt: &'static str,
    c_value: &'static str,
    velt_value: &'static str,
}

const fn f(
    c: &'static str,
    velt: &'static str,
    c_value: &'static str,
    velt_value: &'static str,
) -> Field {
    Field {
        c,
        velt,
        c_value,
        velt_value,
    }
}

const I8: Field = f("int8_t", "i8", "(int8_t)(B % 100)", "(B % 100) as i8");
const U8: Field = f("uint8_t", "u8", "(uint8_t)(B % 200)", "(B % 200) as u8");
const I16: Field = f(
    "int16_t",
    "i16",
    "(int16_t)(B % 30000)",
    "(B % 30000) as i16",
);
const U16: Field = f(
    "uint16_t",
    "u16",
    "(uint16_t)(B % 60000)",
    "(B % 60000) as u16",
);
const I32: Field = f("int32_t", "i32", "(int32_t)(B * 7)", "(B * 7) as i32");
const U32: Field = f("uint32_t", "u32", "(uint32_t)(B * 9)", "(B * 9) as u32");
const I64: Field = f("int64_t", "i64", "(int64_t)(B * 12345)", "B * 12345");
const U64: Field = f("uint64_t", "u64", "(uint64_t)(B * 777)", "(B * 777) as u64");
const F32: Field = f(
    "float",
    "f32",
    "(float)(B % 1000) / 4.0f",
    "((B % 1000) as f32) / 4",
);
const F64: Field = f("double", "f64", "(double)B / 8.0", "(B as f64) / 8");
const BOOL: Field = f("_Bool", "bool", "(B % 2) == 1", "(B % 2) == 1");
const PTR: Field = f(
    "void *",
    "Ptr<void> | null",
    "(void *)(uintptr_t)(B * 16)",
    "ptrOf(B * 16)",
);

/// The shapes: each struct's fields, in order. Nested structs are `Inner`, defined first.
fn shapes() -> Vec<Vec<&'static Field>> {
    vec![
        vec![&F64, &F64],                   // SSE, SSE
        vec![&I64, &F64],                   // INTEGER, SSE
        vec![&F64, &I64],                   // SSE, INTEGER
        vec![&F32, &F32],                   // one SSE eightbyte
        vec![&F32, &F32, &F32],             // SSE, SSE (12 bytes)
        vec![&F32, &F32, &F32, &F32],       // SSE, SSE
        vec![&U8, &I32],                    // one INTEGER eightbyte
        vec![&I8, &I16, &I32, &I64],        // INTEGER, INTEGER, padding inside
        vec![&I64, &I64, &I64],             // MEMORY (24 bytes)
        vec![&BOOL, &F32],                  // INTEGER: a bool shares it
        vec![&F64, &F64, &F64],             // MEMORY of floats
        vec![&U8],                          // one byte
        vec![&I32, &F32],                   // INTEGER: an integer shares it
        vec![&I16, &U16, &I16],             // six bytes
        vec![&PTR, &U64],                   // a pointer: INTEGER
        vec![&U32, &F64, &U8],              // INTEGER, SSE, INTEGER: 24 bytes, MEMORY
        vec![&I32, &I32, &I32, &I32, &I32], // MEMORY of 20 bytes: no whole eightbytes
        vec![&U8; 17],                      // MEMORY of 17 bytes
        vec![&F32],                         // four bytes, SSE
        vec![&I32],                         // four bytes, INTEGER
    ]
}

fn c_source(shapes: &[Vec<&Field>]) -> (String, String) {
    let mut h = String::from("#include <stdint.h>\n#include <stdbool.h>\n\n");
    let mut c = String::from("#include \"abi.h\"\n\n");
    for (i, fields) in shapes.iter().enumerate() {
        let _ = write!(h, "typedef struct S{i} {{");
        for (j, fd) in fields.iter().enumerate() {
            let _ = write!(h, " {} f{j};", fd.c);
        }
        let _ = writeln!(h, " }} S{i};");
        let _ = writeln!(h, "S{i} mk{i}(int64_t seed);\nint64_t chk{i}(S{i} v, int64_t seed);\nS{i} echo{i}(S{i} v);");
        let _ = writeln!(c, "S{i} mk{i}(int64_t seed) {{\n  S{i} v;");
        for (j, fd) in fields.iter().enumerate() {
            let value = fd
                .c_value
                .replace('B', &format!("(seed * 1000 + {j} * 10 + {i})"));
            let _ = writeln!(c, "  v.f{j} = {value};");
        }
        let _ = writeln!(
            c,
            "  return v;\n}}\nint64_t chk{i}(S{i} v, int64_t seed) {{\n  S{i} w = mk{i}(seed);"
        );
        for j in 0..fields.len() {
            let _ = writeln!(c, "  if (v.f{j} != w.f{j}) return -{};", j + 1);
        }
        let _ = writeln!(c, "  return 1;\n}}\nS{i} echo{i}(S{i} v) {{ return v; }}\n");
    }
    // Registers running out: S7 (two INTEGER eightbytes) after five integers goes on the stack,
    // S0 (two SSE) after seven doubles too; a result in memory (S8) takes the first integer
    // register, so S7 after four integers goes on the stack.
    h.push_str("int64_t late_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, S7 s, int64_t f);\n");
    h.push_str("int64_t late_sse(double a, double b, double c, double d, double e, double f, double g, S0 s, double h);\n");
    // Structs whose size is no whole number of eightbytes on the stack: a 4-byte one after the
    // vector and after the integer registers run out, and a 12-byte one (two SSE) with one
    // vector register left.
    h.push_str("int64_t late_small_sse(double a, double b, double c, double d, double e, double f, double g, double h, S18 s);\n");
    h.push_str("int64_t late_small_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, S19 s);\n");
    h.push_str("int64_t late_f3(double a, double b, double c, double d, double e, double f, double g, S4 s);\n");
    h.push_str("S8 big_after(int64_t a, int64_t b, int64_t c, int64_t d, S7 s);\n");
    h.push_str("typedef struct NP { void *p; int64_t n; } NP;\nNP null_ptr(void);\n");
    c.push_str("int64_t late_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, S7 s, int64_t f) {\n  if (a + b + c + d + e + f != 21) return -100;\n  return chk7(s, 3);\n}\n");
    c.push_str("int64_t late_sse(double a, double b, double c, double d, double e, double f, double g, S0 s, double h) {\n  if (a + b + c + d + e + f + g + h != 36.0) return -100;\n  return chk0(s, 4);\n}\n");
    c.push_str("int64_t late_small_sse(double a, double b, double c, double d, double e, double f, double g, double h, S18 s) {\n  if (a + b + c + d + e + f + g + h != 36.0) return -100;\n  return chk18(s, 6);\n}\n");
    c.push_str("int64_t late_small_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, S19 s) {\n  if (a + b + c + d + e + f != 21) return -100;\n  return chk19(s, 7);\n}\n");
    c.push_str("int64_t late_f3(double a, double b, double c, double d, double e, double f, double g, S4 s) {\n  if (a + b + c + d + e + f + g != 28.0) return -100;\n  return chk4(s, 8);\n}\n");
    c.push_str("NP null_ptr(void) {\n  NP r = { 0, 7 };\n  return r;\n}\n");
    c.push_str("S8 big_after(int64_t a, int64_t b, int64_t c, int64_t d, S7 s) {\n  S8 r = mk8(a + b + c + d);\n  if (chk7(s, 5) != 1) r.f0 = -1;\n  return r;\n}\n");
    (h, c)
}

fn velt_lib(shapes: &[Vec<&Field>]) -> String {
    let mut v = String::from("import { Ptr } from \"velt/ffi\";\n\nfunction ptrOf(a: i64): Ptr<void> | null {\n  // SAFETY: never read through; an address made from a number.\n  return unsafe { Ptr.orNull<void>(a as usize) };\n}\n\n");
    for (i, fields) in shapes.iter().enumerate() {
        let _ = write!(v, "export declare c struct S{i} {{");
        for (j, fd) in fields.iter().enumerate() {
            let _ = write!(v, " f{j}: {};", fd.velt);
        }
        let _ = writeln!(v, " }}");
        let _ = writeln!(v, "declare c(\"abi\") function mk{i}(seed: i64): S{i};\ndeclare c(\"abi\") function chk{i}(v: S{i}, seed: i64): i64;\ndeclare c(\"abi\") function echo{i}(v: S{i}): S{i};");
        let _ = writeln!(v, "function make{i}(seed: i64): S{i} {{\n  return {{");
        for (j, fd) in fields.iter().enumerate() {
            let value = fd
                .velt_value
                .replace('B', &format!("(seed * 1000 + {j} * 10 + {i})"));
            let _ = writeln!(v, "    f{j}: {value},");
        }
        let _ = writeln!(v, "  }};\n}}");
        // Velt to C (`chk`), C to Velt (`mk`), and both (`echo`).
        let _ = writeln!(v, "function shape{i}(seed: i64): string {{\n  const want = make{i}(seed);\n  // SAFETY: the C functions take and return the struct by value, nothing else.\n  const [sent, got, back] = unsafe {{ [chk{i}(want, seed), mk{i}(seed), echo{i}(want)] }};\n  return `S{i}: sent ${{sent}}, received ${{got === want}}, echoed ${{back === want}}`;\n}}\n");
    }
    v.push_str("declare c(\"abi\") function late_int(a: i64, b: i64, c: i64, d: i64, e: i64, s: S7, f: i64): i64;\n");
    v.push_str("declare c(\"abi\") function late_sse(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, s: S0, h: f64): i64;\n");
    v.push_str("declare c(\"abi\") function late_small_sse(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, s: S18): i64;\n");
    v.push_str("declare c(\"abi\") function late_small_int(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, s: S19): i64;\n");
    v.push_str("declare c(\"abi\") function late_f3(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, s: S4): i64;\n");
    v.push_str(
        "declare c(\"abi\") function big_after(a: i64, b: i64, c: i64, d: i64, s: S7): S8;\n\n",
    );
    v.push_str("export declare c struct NP { p: Ptr<void>; n: i64; }\ndeclare c(\"abi\") function null_ptr(): NP;\n");
    v.push_str("export function nullField(): i64 {\n  // SAFETY: by value; C leaves `p` NULL.\n  return unsafe { null_ptr() }.n;\n}\n\n");
    v.push_str("export function all(): string[] {\n  const out: string[] = [];\n");
    for i in 0..shapes.len() {
        let _ = writeln!(v, "  out.push(shape{i}(1));\n  out.push(shape{i}(-2));");
    }
    v.push_str("  // SAFETY: as above.\n  unsafe {\n    out.push(`late_int ${late_int(1, 2, 3, 4, 5, make7(3), 6)}`);\n    out.push(`late_sse ${late_sse(1, 2, 3, 4, 5, 6, 7, make0(4), 8)}`);\n    out.push(`big_after ${big_after(1, 2, 3, 4, make7(5)) === make8(10)}`);\n    out.push(`late_small_sse ${late_small_sse(1, 2, 3, 4, 5, 6, 7, 8, make18(6))}`);\n    out.push(`late_small_int ${late_small_int(1, 2, 3, 4, 5, 6, make19(7))}`);\n    out.push(`late_f3 ${late_f3(1, 2, 3, 4, 5, 6, 7, make4(8))}`);\n  }\n  ;\n  return out;\n}\n");
    v
}

fn project(dir: &Path, main: &str) -> PathBuf {
    let shapes = shapes();
    let (h, c) = c_source(&shapes);
    let pkg = dir.join("abi");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"abi\", version: \"0.1.0\", native: { c: { abi: { sources: [\"native/abi.c\"], headers: [\"native/abi.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("native/abi.h"), h).unwrap();
    std::fs::write(pkg.join("native/abi.c"), c).unwrap();
    std::fs::write(pkg.join("src/lib.velt"), velt_lib(&shapes)).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"abi\"],\n  dependencies: { abi: { path: \"../abi\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
    app
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    home
}

/// What every line prints when both directions agree.
fn expected() -> String {
    let mut out = String::new();
    for i in 0..shapes().len() {
        for _ in 0..2 {
            let _ = writeln!(out, "S{i}: sent 1, received true, echoed true");
        }
    }
    out.push_str(
        "late_int 1\nlate_sse 1\nbig_after true\nlate_small_sse 1\nlate_small_int 1\nlate_f3 1\n",
    );
    out
}

#[test]
fn structs_cross_to_c_and_back_by_value_in_every_build() {
    let Some(_zig) = zig_env::zig("build C sources taking structs by value") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { all } from \"abi\";\n\nfunction main() {\n  for (const line of all()) {\n    console.log(line);\n  }\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    for args in [&["run", "--release"][..], &["run"][..]] {
        let o = velt(&app, &home, args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "velt {args:?}:\n{out}");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            expected(),
            "velt {args:?}:\n{out}"
        );
    }
}

/// A struct result arriving from C has its non-null pointer fields checked, as `Ptr.read` does:
/// C left `p` NULL, so the call stops the program, naming the field.
#[test]
fn a_null_in_a_non_null_pointer_field_of_a_result_stops_the_call() {
    let Some(_zig) = zig_env::zig("build C sources returning a struct with a NULL field") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { nullField } from \"abi\";\n\nfunction main() {\n  console.log(\"before\");\n  console.log(nullField());\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    for args in [&["run", "--release"][..], &["run"][..]] {
        let o = velt(&app, &home, args);
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(o.status.code(), Some(101), "velt {args:?}:\n{out}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "before\n", "{out}");
        assert!(
            out.contains("C function `null_ptr`: field `p` of the `NP` it returned is NULL"),
            "velt {args:?}:\n{out}"
        );
    }
}
