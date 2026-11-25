//! C structs passed to and returned from C functions by value (#2187 layer 3,
//! docs/internals/design/c-ffi.md "C structs"): a differential test of the target's C calling
//! convention. A package's C `sources`, compiled by zig's clang, define for each struct shape a
//! function that builds one from a seed, one that checks every field of one it is given, and one
//! that returns what it is given; Velt builds the same values, so both directions are compared
//! field by field, in debug (Cranelift) and release (LLVM) builds. Further functions take structs
//! after the registers run out (a struct takes all its registers or goes on the stack whole), and
//! return one in memory with arguments around it. These tests need `VELT_ZIG` as
//! tests/c_sources.rs's do, and run on the hosts whose convention lowering has (x86-64 Linux,
//! macOS and Windows, arm64 Linux and macOS).
//!
//! **Another target's convention**: with `VELT_ABI_TARGET` set to a triple, the programs are
//! built for it (`velt build --target`, whose runtime and linker `VELT_RT_LIB` and
//! `VELT_LINKER` can name) and run with `VELT_ABI_RUNNER` (an emulator, its words split at
//! whitespace: `qemu-aarch64-static`, `wine`), so one Linux machine checks every convention
//! (docs/internals/design/c-ffi.md "C structs").

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
        vec![&U8, &U8, &U8],                // three bytes: by reference on Win64
        vec![&F64, &F64, &F64, &F64],       // an HFA of four on arm64, MEMORY on System V
        vec![&F32, &F64],                   // mixed floats: no HFA
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
    // AAPCS64: a struct that doesn't fit in the registers left goes on the stack, and the
    // scalars of its class after it too (an HFA after six doubles, a pair after seven
    // integers, three floats after seven doubles with a float after them); a struct by
    // reference after the eight integer registers has its address on the stack.
    h.push_str("int64_t late_hfa(double a, double b, double c, double d, double e, double f, S5 s, double g);\n");
    h.push_str("int64_t late_pair(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, S7 s, int64_t h);\n");
    h.push_str("int64_t late_f3_float(double a, double b, double c, double d, double e, double f, double g, S4 s, float h);\n");
    h.push_str("int64_t late_big(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, int64_t h, S8 s, int64_t i);\n");
    h.push_str("typedef struct NP { void *p; int64_t n; } NP;\nNP null_ptr(void);\n");
    // `Out<S>` (an `S *` C fills) of a struct with padding and a nested struct, whose `bool` C
    // sets to the byte 2, and `Out<bool>` set the same way: Velt reads each `bool` as its byte's
    // truth.
    h.push_str("typedef struct OI { int16_t b; double c; } OI;\ntypedef struct OS { uint8_t a; OI inner; uint32_t d; _Bool flag; void *p; } OS;\nint32_t out_fill(int32_t seed, OS *out);\nvoid out_bool(_Bool *b);\n");
    // `Ref<S>` (a `const S *` to a copy): C checks every field; writes through it (casting the
    // `const` away), which reaches the copy only; and takes `NULL` for a `Ref<S> | null`.
    h.push_str("int32_t ref_check(const OS *s, int32_t seed);\nint32_t ref_poke(const OS *s);\nint32_t ref_maybe(const OS *s);\n");
    // A `Ref` through type aliases (an 8-byte struct, which by value would cross in a register
    // where C reads a pointer): `type S6Ref = Ref<S6>`, and an alias of `Ref<S6> | null`.
    h.push_str(
        "int32_t ref_alias(const S6 *s, int64_t seed);\nint32_t ref_alias_maybe(const S6 *s);\nint32_t ref_id(const S6 *s, int64_t seed);\nint32_t ref_id_maybe(const S6 *s);\n",
    );
    // A `Ref` as the fixed parameter of a variadic C function: a pointer, as any other.
    h.push_str("#include <stdarg.h>\nint32_t ref_var(const OS *s, int32_t n, ...);\n");
    // Arrays of C structs: C reads every field of every element of a `readonly OS[]` (an empty
    // one too), and fills an `OI[]` in place.
    h.push_str("int64_t arr_sum(const OS *xs, int32_t n);\nvoid arr_fill(OI *xs, int32_t n);\ntypedef struct PN { void *p; int64_t n; } PN;\nvoid pn_fill(PN *xs, int32_t n);\n");
    c.push_str("int64_t late_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, S7 s, int64_t f) {\n  if (a + b + c + d + e + f != 21) return -100;\n  return chk7(s, 3);\n}\n");
    c.push_str("int64_t late_sse(double a, double b, double c, double d, double e, double f, double g, S0 s, double h) {\n  if (a + b + c + d + e + f + g + h != 36.0) return -100;\n  return chk0(s, 4);\n}\n");
    c.push_str("int64_t late_small_sse(double a, double b, double c, double d, double e, double f, double g, double h, S18 s) {\n  if (a + b + c + d + e + f + g + h != 36.0) return -100;\n  return chk18(s, 6);\n}\n");
    c.push_str("int64_t late_small_int(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, S19 s) {\n  if (a + b + c + d + e + f != 21) return -100;\n  return chk19(s, 7);\n}\n");
    c.push_str("int64_t late_f3(double a, double b, double c, double d, double e, double f, double g, S4 s) {\n  if (a + b + c + d + e + f + g != 28.0) return -100;\n  return chk4(s, 8);\n}\n");
    c.push_str("int64_t late_hfa(double a, double b, double c, double d, double e, double f, S5 s, double g) {\n  if (a + b + c + d + e + f + g != 28.0) return -100;\n  return chk5(s, 9);\n}\n");
    c.push_str("int64_t late_pair(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, S7 s, int64_t h) {\n  if (a + b + c + d + e + f + g + h != 36) return -100;\n  return chk7(s, 10);\n}\n");
    c.push_str("int64_t late_f3_float(double a, double b, double c, double d, double e, double f, double g, S4 s, float h) {\n  if (a + b + c + d + e + f + g + h != 36.0) return -100;\n  return chk4(s, 11);\n}\n");
    c.push_str("int64_t late_big(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, int64_t h, S8 s, int64_t i) {\n  if (a + b + c + d + e + f + g + h + i != 45) return -100;\n  return chk8(s, 12);\n}\n");
    c.push_str("int32_t out_fill(int32_t seed, OS *out) {\n  out->a = (uint8_t)(seed + 1);\n  out->inner.b = (int16_t)(seed * 3);\n  out->inner.c = seed / 4.0;\n  out->d = (uint32_t)(seed * 1000);\n  *(uint8_t *)&out->flag = 2;\n  out->p = (void *)(uintptr_t)(seed * 16);\n  return seed;\n}\n");
    c.push_str("void out_bool(_Bool *b) { *(uint8_t *)b = 2; }\n");
    c.push_str("int32_t ref_check(const OS *s, int32_t seed) {\n  if (s->a != (uint8_t)(seed + 1)) return -1;\n  if (s->inner.b != seed * 3) return -2;\n  if (s->inner.c != seed / 4.0) return -3;\n  if (s->d != (uint32_t)(seed * 1000)) return -4;\n  if (!s->flag) return -5;\n  if (s->p != (void *)(uintptr_t)(seed * 16)) return -6;\n  return 1;\n}\n");
    c.push_str("int32_t ref_poke(const OS *s) {\n  ((OS *)s)->a = 99;\n  return s->a;\n}\n");
    c.push_str("int32_t ref_maybe(const OS *s) { return !s ? 0 : s->a; }\n");
    c.push_str(
        "int32_t ref_alias(const S6 *s, int64_t seed) { return (int32_t)chk6(*s, seed); }\n",
    );
    c.push_str("int32_t ref_alias_maybe(const S6 *s) { return !s ? 0 : 1; }\n");
    c.push_str("int32_t ref_id(const S6 *s, int64_t seed) { return (int32_t)chk6(*s, seed); }\nint32_t ref_id_maybe(const S6 *s) { return !s ? 0 : 1; }\n");
    c.push_str("int32_t ref_var(const OS *s, int32_t n, ...) {\n  va_list ap;\n  va_start(ap, n);\n  int32_t sum = s->a;\n  for (int32_t i = 0; i < n; i++) sum += va_arg(ap, int32_t);\n  va_end(ap);\n  return sum;\n}\n");
    c.push_str("int64_t arr_sum(const OS *xs, int32_t n) {\n  int64_t s = 0;\n  for (int32_t i = 0; i < n; i++) s += xs[i].a + xs[i].inner.b + (int64_t)(xs[i].inner.c * 2) + xs[i].d + xs[i].flag + (xs[i].p != 0);\n  return s;\n}\n");
    c.push_str("void arr_fill(OI *xs, int32_t n) {\n  for (int32_t i = 0; i < n; i++) {\n    xs[i].b = (int16_t)(i * 7 + 1);\n    xs[i].c = i + 0.5;\n  }\n}\n");
    c.push_str("void pn_fill(PN *xs, int32_t n) {\n  for (int32_t i = 0; i < n; i++) {\n    xs[i].p = i % 2 ? (void *)(uintptr_t)(64 * i) : 0;\n    xs[i].n = i * 3;\n  }\n}\n");
    c.push_str("NP null_ptr(void) {\n  NP r = { 0, 7 };\n  return r;\n}\n");
    c.push_str("S8 big_after(int64_t a, int64_t b, int64_t c, int64_t d, S7 s) {\n  S8 r = mk8(a + b + c + d);\n  if (chk7(s, 5) != 1) r.f0 = -1;\n  return r;\n}\n");
    (h, c)
}

fn velt_lib(shapes: &[Vec<&Field>]) -> String {
    let mut v = String::from("import { CVarArgs, Out, Ptr, Ref } from \"velt/ffi\";\n\nfunction ptrOf(a: i64): Ptr<void> | null {\n  // SAFETY: never read through; an address made from a number.\n  return unsafe { Ptr.orNull<void>(a as usize) };\n}\n\n");
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
    v.push_str("declare c(\"abi\") function late_hfa(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, s: S5, g: f64): i64;\n");
    v.push_str("declare c(\"abi\") function late_pair(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, s: S7, h: i64): i64;\n");
    v.push_str("declare c(\"abi\") function late_f3_float(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, s: S4, h: f32): i64;\n");
    v.push_str("declare c(\"abi\") function late_big(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, s: S8, i: i64): i64;\n");
    v.push_str("export declare c struct NP { p: Ptr<void>; n: i64; }\ndeclare c(\"abi\") function null_ptr(): NP;\n");
    v.push_str("export declare c struct OI { b: i16; c: f64; }\nexport declare c struct OS { a: u8; inner: OI; d: u32; flag: bool; p: Ptr<void> | null; }\n");
    v.push_str("declare c(\"abi\") function out_fill(seed: i32, out: Out<OS>): i32;\ndeclare c(\"abi\") function out_bool(b: Out<bool>): void;\n");
    v.push_str("declare c(\"abi\") function ref_check(s: Ref<OS>, seed: i32): i32;\ndeclare c(\"abi\") function ref_poke(s: Ref<OS>): i32;\ndeclare c(\"abi\") function ref_maybe(s: Ref<OS> | null): i32;\n");
    v.push_str("type S6Ref = Ref<S6>;\ntype MaybeS6 = S6Ref | null;\ndeclare c(\"abi\") function ref_alias(s: S6Ref, seed: i64): i32;\ndeclare c(\"abi\") function ref_alias_maybe(s: MaybeS6): i32;\n");
    // Through generic aliases whose body is their parameter: `Id<Ref<S6>>` is the `Ref`.
    v.push_str("type Id<T> = T;\ntype Maybe<T> = T | null;\ndeclare c(\"abi\") function ref_id(s: Id<Ref<S6>>, seed: i64): i32;\ndeclare c(\"abi\") function ref_id_maybe(s: Maybe<Id<Ref<S6>>>): i32;\n");
    v.push_str(
        "declare c(\"abi\") function ref_var(s: Ref<OS>, n: i32, ...args: CVarArgs): i32;\n",
    );
    v.push_str("export function refs(): string[] {\n  const s: OS = { a: 6, inner: { b: 15, c: 1.25 }, d: 5000, flag: true, p: ptrOf(80) };\n  // SAFETY: each reads the struct during the call and keeps no pointer to it.\n  const [checked, poked, some, none] = unsafe { [ref_check(s, 5), ref_poke(s), ref_maybe(s), ref_maybe(null)] };\n  // SAFETY: as above.\n  const [aliased, aliasSome, aliasNone] = unsafe { [ref_alias(make6(3), 3), ref_alias_maybe(make6(3)), ref_alias_maybe(null)] };\n  // SAFETY: as above.\n  const [ided, idSome, idNone] = unsafe { [ref_id(make6(4), 4), ref_id_maybe(make6(4)), ref_id_maybe(null)] };\n  // SAFETY: as above; the variadic arguments are two `int32_t`s.\n  const summed = unsafe { ref_var(s, 2, 10 as i32, 20 as i32) };\n  return [`ref_check ${checked}`, `ref_poke ${poked} ${s.a}`, `ref_maybe ${some} ${none}`, `ref_alias ${aliased} ${aliasSome} ${aliasNone}`, `ref_id ${ided} ${idSome} ${idNone}`, `ref_var ${summed}`];\n}\n");
    v.push_str("declare c(\"abi\") function arr_sum(xs: readonly OS[], n: i32): i64;\ndeclare c(\"abi\") function arr_fill(xs: OI[], n: i32): void;\nexport declare c struct PN { p: Ptr<void> | null; n: i64; }\ndeclare c(\"abi\") function pn_fill(xs: PN[], n: i32): void;\n");
    v.push_str("export function arrays(): string[] {\n  const xs: OS[] = [{ a: 1, inner: { b: 2, c: 0.5 }, d: 3, flag: true, p: ptrOf(16) }, { a: 10, inner: { b: 20, c: 0 }, d: 30, flag: false, p: null }, { a: 100, inner: { b: 200, c: 0 }, d: 300, flag: true, p: null }];\n  const ys: OI[] = [{ b: 0, c: 0 }, { b: 0, c: 0 }, { b: 0, c: 0 }];\n  const empty: OS[] = [];\n  // SAFETY: C reads `n` elements of each during the call and keeps no pointer to them.\n  const [sum, none] = unsafe { [arr_sum(xs, xs.length as i32), arr_sum(empty, 0)] };\n  // SAFETY: C writes `n` elements of `ys` during the call.\n  unsafe { arr_fill(ys, ys.length as i32) };\n  const ps: PN[] = [{ p: ptrOf(8), n: 0 }, { p: null, n: 0 }];\n  // SAFETY: C writes `n` elements of `ps` during the call: `NULL`, then an address.\n  unsafe { pn_fill(ps, ps.length as i32) };\n  return [`arr_sum ${sum} ${none}`, `arr_fill ${ys[0].b} ${ys[1].b} ${ys[2].b} ${ys[2].c}`, `pn_fill ${ps[0].p === null} ${ps[1].p === ptrOf(64)} ${ps[1].n}`];\n}\n");
    v.push_str("function truth(b: bool): string {\n  return `${b} ${!b} ${b === true}`;\n}\n");
    v.push_str("export function outs(): string[] {\n  const o = new Out<OS>({ a: 0, inner: { b: 0, c: 0 }, d: 0, flag: false, p: null });\n  // SAFETY: `o` is the struct it fills.\n  const r = unsafe { out_fill(5, o) };\n  const s = o.value;\n  const same = r === 5 && s.a === 6 && s.inner.b === 15 && s.inner.c === 1.25 && s.d === 5000 && s.p === ptrOf(80);\n  const b = new Out<bool>(false);\n  // SAFETY: `b` is the bool it sets.\n  unsafe { out_bool(b) };\n  return [`out_struct ${same}`, `out_flag ${truth(s.flag)}`, `out_bool ${truth(b.value)}`];\n}\n\n");
    v.push_str("export function nullField(): i64 {\n  // SAFETY: by value; C leaves `p` NULL.\n  return unsafe { null_ptr() }.n;\n}\n\n");
    v.push_str("export function all(): string[] {\n  const out: string[] = [];\n");
    for i in 0..shapes.len() {
        let _ = writeln!(v, "  out.push(shape{i}(1));\n  out.push(shape{i}(-2));");
    }
    v.push_str("  // SAFETY: as above.\n  unsafe {\n    out.push(`late_int ${late_int(1, 2, 3, 4, 5, make7(3), 6)}`);\n    out.push(`late_sse ${late_sse(1, 2, 3, 4, 5, 6, 7, make0(4), 8)}`);\n    out.push(`big_after ${big_after(1, 2, 3, 4, make7(5)) === make8(10)}`);\n    out.push(`late_small_sse ${late_small_sse(1, 2, 3, 4, 5, 6, 7, 8, make18(6))}`);\n    out.push(`late_small_int ${late_small_int(1, 2, 3, 4, 5, 6, make19(7))}`);\n    out.push(`late_f3 ${late_f3(1, 2, 3, 4, 5, 6, 7, make4(8))}`);\n    out.push(`late_hfa ${late_hfa(1, 2, 3, 4, 5, 6, make5(9), 7)}`);\n    out.push(`late_pair ${late_pair(1, 2, 3, 4, 5, 6, 7, make7(10), 8)}`);\n    out.push(`late_f3_float ${late_f3_float(1, 2, 3, 4, 5, 6, 7, make4(11), 8)}`);\n    out.push(`late_big ${late_big(1, 2, 3, 4, 5, 6, 7, 8, make8(12), 9)}`);\n  }\n  ;\n  for (const line of outs()) {\n    out.push(line);\n  }\n  for (const line of refs()) {\n    out.push(line);\n  }\n  for (const line of arrays()) {\n    out.push(line);\n  }\n  return out;\n}\n");
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

/// `velt run` with `args`, or with `VELT_ABI_TARGET` set, the same program built for that target
/// and run with `VELT_ABI_RUNNER`.
fn run(app: &Path, home: &Path, args: &[&str]) -> Output {
    let Some(target) = std::env::var("VELT_ABI_TARGET")
        .ok()
        .filter(|t| !t.is_empty())
    else {
        return velt(app, home, args);
    };
    let exe = app.join(match target.contains("windows") {
        true => "abi-test.exe",
        false => "abi-test",
    });
    let mut build = vec!["build", "--target", &target, "-o"];
    let out = exe.to_string_lossy().into_owned();
    build.push(&out);
    build.extend(args.iter().filter(|&&a| a == "--release"));
    let o = velt(app, home, &build);
    if !o.status.success() {
        return o;
    }
    let runner = std::env::var("VELT_ABI_RUNNER").unwrap_or_default();
    let mut words = runner.split_whitespace();
    let mut cmd = match words.next() {
        Some(r) => {
            let mut c = no_window::command(r);
            c.args(words).arg(&exe);
            c
        }
        None => no_window::command(&exe),
    };
    cmd.current_dir(app).env("NO_COLOR", "1").output().unwrap()
}

/// Can this host run what `velt run` builds and the test compares: is its C convention for
/// structs by value lowered, or is another target's named?
fn runs_here() -> bool {
    let native = cfg!(any(
        all(target_arch = "x86_64", any(unix, windows)),
        all(
            target_arch = "aarch64",
            any(target_os = "linux", target_os = "macos")
        )
    ));
    native || std::env::var("VELT_ABI_TARGET").is_ok_and(|t| !t.is_empty())
}

fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    #[cfg(not(unix))]
    let _ = shared;
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
        "late_int 1\nlate_sse 1\nbig_after true\nlate_small_sse 1\nlate_small_int 1\nlate_f3 1\nlate_hfa 1\nlate_pair 1\nlate_f3_float 1\nlate_big 1\nout_struct true\nout_flag true false true\nout_bool true false true\nref_check 1\nref_poke 99 6\nref_maybe 6 0\nref_alias 1 1 0\nref_id 1 1 0\nref_var 36\narr_sum 670 0\narr_fill 1 8 15 2.5\npn_fill true true 3\n",
    );
    out
}

#[test]
fn structs_cross_to_c_and_back_by_value_in_every_build() {
    let Some(_zig) = zig_env::zig("build C sources taking structs by value") else {
        return;
    };
    if !runs_here() {
        return;
    }
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { all } from \"abi\";\n\nfunction main() {\n  for (const line of all()) {\n    console.log(line);\n  }\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    for args in [&["run", "--release"][..], &["run"][..]] {
        let o = run(&app, &home, args);
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
    if !runs_here() {
        return;
    }
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { nullField } from \"abi\";\n\nfunction main() {\n  console.log(\"before\");\n  console.log(nullField());\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    for args in [&["run", "--release"][..], &["run"][..]] {
        let o = run(&app, &home, args);
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

/// The conventions this machine can't run (Win64, Apple's arm64, and arm64 Linux without an
/// emulator): every C function's signature in the LLVM IR of a release build for the target, as
/// clang (zig's) declares the same C function for it. A struct in registers is its words where
/// clang has the struct's type, an array or a narrower integer, the same registers. The
/// functions taking a struct on the arm64 stack are left out there: lowering passes zeros in the
/// registers left and the struct's words after them, where clang has one array
/// (c_struct_abi/aapcs64.rs), which only a run checks (`VELT_ABI_TARGET`).
#[test]
fn signatures_match_clangs_on_the_other_targets() {
    let Some(zig) = zig_env::zig("compare C struct signatures with clang's") else {
        return;
    };
    let dir = test_dir::TestDir::new();
    let main = "import { all, nullField } from \"abi\";\n\nfunction main() {\n  console.log(all().length + nullField());\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    let c = dir.path().join("abi/native/abi.c");
    let on_the_arm64_stack = [
        "late_sse",
        "late_f3",
        "late_f3_float",
        "late_hfa",
        "late_pair",
        "late_small_sse",
    ];
    for (target, zig_target) in [
        ("x86_64-pc-windows-msvc", "x86_64-windows-gnu"),
        ("aarch64-apple-darwin", "aarch64-macos"),
        ("aarch64-unknown-linux-gnu", "aarch64-linux-gnu"),
    ] {
        let o = velt(
            &app,
            &home,
            &["build", "--release", "--emit", "llvm", "--target", target],
        );
        let ir = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(
            o.status.success(),
            "{target}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let ll = dir.path().join(format!("{zig_target}.ll"));
        let o = no_window::command(&zig)
            .args(["cc", "-target", zig_target, "-ffreestanding", "-O1", "-S"])
            .arg("-emit-llvm")
            .arg("-o")
            .arg(&ll)
            .arg(&c)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let theirs = signatures(&std::fs::read_to_string(&ll).unwrap(), "define");
        let ours = signatures(&ir, "declare");
        let mut compared = 0;
        for (name, sig) in &theirs {
            if target.starts_with("aarch64") && on_the_arm64_stack.contains(&name.as_str()) {
                continue;
            }
            let ours = ours
                .get(name)
                .unwrap_or_else(|| panic!("{target}: no `{name}` in Velt's IR"));
            assert_eq!(ours, sig, "{target}: `{name}` (Velt's, clang's)");
            compared += 1;
        }
        assert!(compared > 60, "{target}: only {compared} compared");
    }
}

/// The C functions of the test's library in LLVM IR `ir` (lines starting with `kw`), each with
/// its result's and parameters' registers: `int`, `float`, `double`, `ptr`, `sret`, or `hfa` for a
/// result of floats only.
fn signatures(
    ir: &str,
    kw: &str,
) -> std::collections::BTreeMap<String, (Vec<String>, Vec<String>)> {
    let mut out = std::collections::BTreeMap::new();
    for line in ir.lines().filter(|l| l.starts_with(kw)) {
        let Some(at) = line.find('@') else { continue };
        let rest = &line[at + 1..];
        let Some(open) = rest.find('(') else { continue };
        let name = rest[..open].trim_matches('"').to_string();
        let lib = ["mk", "chk", "echo", "late_", "big_after", "null_ptr"];
        if !lib.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let params = &rest[open + 1..rest.rfind(')').unwrap_or(rest.len())];
        let ret = types(&line[kw.len()..at]).pop().unwrap_or_default();
        let ret = match ret.as_slice() {
            w if !w.is_empty() && w.iter().all(|t| t == "float" || t == "double") => {
                vec!["hfa".to_string()]
            }
            [s] if s.starts_with("%struct.") => vec!["hfa".to_string()],
            _ => ret,
        };
        let ps = split_top(params)
            .into_iter()
            .flat_map(|p| match (p.contains("sret"), p.contains("byval")) {
                (true, _) => vec!["sret".to_string()],
                (_, true) => vec!["byval".to_string()],
                _ => types(&p).into_iter().next().unwrap_or_default(),
            })
            .collect();
        out.insert(name, (ret, ps));
    }
    out
}

/// The types in `s` (attributes and names skipped), each as the registers it takes: an array or
/// a `{ … }` of words expanded, an integer of any width `int`.
fn types(s: &str) -> Vec<Vec<String>> {
    let one = |t: &str| match t {
        t if t.starts_with('i') && t[1..].parse::<u32>().is_ok() => "int".to_string(),
        t => t.to_string(),
    };
    let mut out = vec![];
    let mut rest = s.trim();
    while !rest.is_empty() {
        let (tok, tail) = match rest.as_bytes()[0] {
            b'[' | b'{' | b'(' => {
                // To the matching close, past nested ones (`[2 x [4 x i8]]`).
                let mut depth = 0;
                let end = (rest.char_indices())
                    .find(|&(_, ch)| {
                        match ch {
                            '[' | '{' | '(' => depth += 1,
                            ']' | '}' | ')' => depth -= 1,
                            _ => {}
                        }
                        depth == 0
                    })
                    .map_or(rest.len(), |(e, _)| e + 1);
                (&rest[..end], &rest[end..])
            }
            _ => {
                let end = rest.find([' ', '(']).unwrap_or(rest.len());
                let end = end.max(1);
                (&rest[..end], &rest[end..])
            }
        };
        rest = tail.trim_start();
        if let Some(inner) = tok.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            let (n, t) = inner.split_once(" x ").unwrap_or(("1", inner));
            out.push(vec![one(t.trim()); n.trim().parse().unwrap_or(1)]);
        } else if let Some(inner) = tok.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            out.push(inner.split(',').map(|t| one(t.trim())).collect());
        } else if tok.starts_with('(') || tok.starts_with('%') && !tok.starts_with("%struct.") {
            // An attribute's arguments, or a parameter's name.
        } else if ["ptr", "float", "double", "void"].contains(&tok)
            || tok.starts_with("%struct.")
            || one(tok) == "int"
        {
            out.push(vec![one(tok)]);
        }
    }
    out
}

/// `s` split at its top-level commas.
fn split_top(s: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (vec![], 0, String::new());
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}
