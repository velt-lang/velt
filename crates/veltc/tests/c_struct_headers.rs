//! `declare c struct`s checked against their library's C headers (#2187 layer 4,
//! docs/internals/design/c-ffi.md "Checking declarations against C headers"): each struct a
//! declaration reaches must be one the headers define, with the same fields by name and type,
//! and clang's own size, alignment and offsets. These tests need `VELT_ZIG` as
//! tests/c_headers.rs's do; the comparison of a field's type is unit-tested in `native::headers`.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;
mod zig_env;

/// A package `geo` whose C library `geo` has the header `header` and whose source is `lib`, and
/// a program `app` that allows it.
fn project(dir: &Path, header: &str, lib: &str) {
    let pkg = dir.join("geo");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"geo\", version: \"0.1.0\", native: { c: { geo: { system: { default: \"geo\" }, headers: [\"native/geo.h\"] } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("native/geo.h"), header).unwrap();
    std::fs::write(
        pkg.join("src/lib.velt"),
        format!(
            "import {{ Out, Ptr, Ref }} from \"velt/ffi\";\n\n{lib}\nexport const ready = 1;\n"
        ),
    )
    .unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"geo\"],\n  dependencies: { geo: { path: \"../geo\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.velt"),
        "import { ready } from \"geo\";\n\nfunction main() {\n  console.log(ready);\n}\n",
    )
    .unwrap();
}

fn check(dir: &Path, target: &str) -> Output {
    let home = dir.join("home");
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "--target", target])
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .env("VELT_HOME", &home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

fn output(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn zig() -> Option<PathBuf> {
    zig_env::zig("run clang on C structs")
}

const LINUX: &str = "x86_64-unknown-linux-gnu";

const GOOD_H: &str = r#"#include <stdint.h>
#include <stdbool.h>
typedef struct point { double x; double y; } point;
typedef struct { struct point origin; int32_t tag; const char *label; } box_t;
struct node { struct node *next; uint8_t kind; void (*visit)(struct node *); };
void use_box(box_t *b);
void use_node(struct node *n);
point *make_point(void);
/* One struct spelled both ways: `point_t` and `struct point` name the same definition. */
typedef struct point point_t;
struct seg { point_t a; struct point b; };
void use_seg(struct seg *s);
void use_pt(point_t *p);
/* An out-parameter of a struct (#2187): `Out<point>`. */
int get_point(point_t *out);
/* A struct C only reads (#2187): `Ref<point>`, and `Ref<point> | null`. */
void draw_point(const point_t *p);
int maybe_point(const struct point *p);
/* Arrays of structs (#2187): `readonly point[]` C reads (also without `const`), `point[]` C
   fills. */
double sum_points(const point_t *ps, int32_t n);
double sum_loose(struct point *ps, int32_t n);
void fill_points(struct point *ps, int32_t n);
/* As arrays of numbers, `void *` takes them (`fwrite`, `qsort`); a struct C writing could make
   no Velt value (a `bool`) is `const` only. */
void put_points(const void *ps, int32_t n);
void take_points(void *ps, int32_t n);
struct flag { bool on; int32_t n; };
void read_flags(const struct flag *xs, int32_t n);
void put_flags(const void *xs, int32_t n);
/* Every kind of field, with padding before and after small ones and a nested struct with tail
   padding: a slip in any width or alignment shows in the offsets. */
#include <stdbool.h>
#include <stddef.h>
struct tail { int64_t big; uint8_t small; };
struct kinds {
  bool flag; int8_t i8; uint8_t u8; int16_t i16; uint16_t u16; float f; int32_t i32;
  uint32_t u32; struct tail t; uint8_t after; void *p; size_t size; ptrdiff_t diff;
  int64_t i64; uint64_t u64; double d; bool last;
};
void use_kinds(struct kinds *k);
"#;

const GOOD: &str = r#"declare c struct point { x: f64; y: f64; }
declare c struct box_t { origin: point; tag: i32; label: Ptr<u8> | null; }
declare c struct node { next: Ptr<node> | null; kind: u8; visit: Ptr<void> | null; }
declare c("geo") function use_box(b: Ptr<box_t>): void;
declare c("geo") function use_node(n: Ptr<node>): void;
declare c("geo") function make_point(): Ptr<point> | null;
declare c struct seg { a: point; b: point; }
declare c("geo") function use_seg(s: Ptr<seg>): void;
declare c("geo") function use_pt(p: Ptr<point>): void;
declare c("geo") function get_point(out: Out<point>): i32;
declare c("geo") function draw_point(p: Ref<point>): void;
declare c("geo") function maybe_point(p: Ref<point> | null): i32;
declare c("geo") function sum_points(ps: readonly point[], n: i32): f64;
declare c("geo") function sum_loose(ps: readonly point[], n: i32): f64;
declare c("geo") function fill_points(ps: point[], n: i32): void;
declare c("geo") function put_points(ps: readonly point[], n: i32): void;
declare c("geo") function take_points(ps: point[], n: i32): void;
declare c struct flag { on: bool; n: i32; }
declare c("geo") function read_flags(xs: readonly flag[], n: i32): void;
declare c("geo") function put_flags(xs: readonly flag[], n: i32): void;
declare c struct tail { big: i64; small: u8; }
declare c struct kinds {
  flag: bool; i8: i8; u8: u8; i16: i16; u16: u16; f: f32; i32: i32;
  u32: u32; t: tail; after: u8; p: Ptr<void> | null; size: usize; diff: isize;
  i64: i64; u64: u64; d: f64; last: bool;
}
declare c("geo") function use_kinds(k: Ptr<kinds>): void;
"#;

/// Structs by tag, by a typedef of a struct without a tag, and with a nested struct, a pointer
/// to the struct itself and a function pointer (`Ptr<void>` until `CFunction`) all pass, on
/// every 64-bit target's headers; so does a struct the header spells both by its tag and by a
/// typedef (as a field and as a pointee), and one with every kind of field.
#[test]
fn structs_matching_their_headers_pass_on_every_target() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    project(dir.path(), GOOD_H, GOOD);
    for target in [LINUX, "aarch64-apple-darwin", "x86_64-pc-windows-msvc"] {
        let o = check(dir.path(), target);
        assert!(o.status.success(), "{target}:\n{}", output(&o));
    }
}

const BAD_H: &str = r#"#include <stdint.h>
struct wrong_type { int32_t a; double b; };
struct renamed { int32_t a; };
struct fewer { int32_t a; int32_t b; };
union either { int32_t a; float b; };
struct hidden;
struct bits { unsigned a : 3; unsigned b : 5; };
struct arr { float m[4]; };
struct packed_s { uint8_t a; uint32_t b; } __attribute__((packed));
struct outer { struct renamed inner; };
void take(struct wrong_type *a, struct renamed *b, struct fewer *c, union either *d,
          struct hidden *e, struct bits *f, struct arr *g, struct packed_s *h, struct outer *i,
          void *j);
"#;

const BAD: &str = r#"declare c struct wrong_type { a: i32; b: f32; }
declare c struct renamed { x: i32; }
declare c struct fewer { a: i32; }
declare c struct either { a: i32; b: f32; }
declare c struct hidden { a: i32; }
declare c struct bits { a: u32; b: u32; }
declare c struct arr { m0: f32; m1: f32; m2: f32; m3: f32; }
declare c struct packed_s { a: u8; b: u32; }
declare c struct elsewhere { a: i32; }
declare c struct outer { inner: elsewhere; }
declare c("geo") function take(
  a: Ptr<wrong_type>,
  b: Ptr<renamed>,
  c: Ptr<fewer>,
  d: Ptr<either>,
  e: Ptr<hidden>,
  f: Ptr<bits>,
  g: Ptr<arr>,
  h: Ptr<packed_s>,
  i: Ptr<outer>,
  j: Ptr<elsewhere>,
): void;
"#;

/// Every way a struct can differ from its header is an error naming the header line: a field's
/// type, its name, the number of fields, a union, a struct only declared, bit-fields, a C array,
/// `packed`, a nested struct of another name, and a struct the headers don't have.
#[test]
fn a_struct_that_differs_from_its_header_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    project(dir.path(), BAD_H, BAD);
    let o = check(dir.path(), LINUX);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for want in [
        "field `b` of C struct `wrong_type` is `double` in geo.h:2, not the declaration's `f32`",
        "field 1 of C struct `renamed` is `a` in geo.h:3, not the declaration's `x`",
        "C struct `fewer` has 2 fields in geo.h:4; the declaration has 1",
        "C struct `either` is a union in geo.h:5",
        "C struct `hidden` is only declared in geo.h:6, never defined",
        "field `a` of C struct `bits` is a bit-field in geo.h:7",
        "C struct `arr` has 1 field in geo.h:8; the declaration has 4",
        "C struct `packed_s` is packed (`__attribute__((packed))`) in geo.h:9",
        "field `inner` of C struct `outer` is `struct renamed` in geo.h:10, not the declaration's `elsewhere`",
        "C struct `elsewhere` is not defined in its library's headers (native/geo.h)",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// A C array field is refused as one even when its name matches, until `T[N]`.
#[test]
fn a_c_array_field_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "struct arr { float m[4]; int32_t n; };\nvoid take(struct arr *a);\n";
    let lib = "declare c struct arr { m: f32; n: i32; }\ndeclare c(\"geo\") function take(a: Ptr<arr>): void;\n";
    project(dir.path(), &format!("#include <stdint.h>\n{header}"), lib);
    let out = output(&check(dir.path(), LINUX));
    assert!(
        out.contains("field `m` of C struct `arr` is `float[4]` in geo.h:2")
            || out.contains("field `m` of C struct `arr` is `float [4]` in geo.h:2"),
        "{out}"
    );
    assert!(out.contains("fixed-size arrays (`T[N]`)"), "{out}");
}

/// The layout is clang's: a struct whose fields match but which `#pragma pack` or `aligned(…)`
/// lays out otherwise is refused, naming the offsets and sizes.
#[test]
fn a_layout_the_headers_change_is_refused() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "#include <stdint.h>\n#pragma pack(push, 2)\nstruct tight { uint8_t a; uint32_t b; };\n#pragma pack(pop)\nstruct wide { uint8_t a; } __attribute__((aligned(16)));\nvoid take(struct tight *t, struct wide *w);\n";
    let lib = "declare c struct tight { a: u8; b: u32; }\ndeclare c struct wide { a: u8; }\ndeclare c(\"geo\") function take(t: Ptr<tight>, w: Ptr<wide>): void;\n";
    project(dir.path(), header, lib);
    let o = check(dir.path(), LINUX);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for want in [
        "C struct `tight` is laid out differently in its headers on x86_64-unknown-linux-gnu: `b` is at offset 2 in C and 4 in Velt; C's `struct tight` is 6 bytes aligned to 2, Velt's 8 aligned to 4",
        "C struct `wide` is laid out differently in its headers on x86_64-unknown-linux-gnu: C's `struct wide` is 16 bytes aligned to 16, Velt's 1 aligned to 1",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// A pointer to a defined struct that isn't the declaration's says to declare the struct, by
/// the name already declared for that definition if there is one, not `declare c type`; a
/// struct only declared is still a `declare c type`.
#[test]
fn a_pointer_to_another_struct_names_the_struct_to_declare() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "#include <stdint.h>\ntypedef struct point { int32_t x; int32_t y; } point_t;\nstruct other { int32_t a; };\nstruct hidden;\nvoid use_other(struct other *p);\nvoid use_other_t(point_t *p);\nvoid use_hidden(struct hidden *p);\nstruct lone { int32_t a; };\nvoid use_lone(struct lone *p);\n";
    let lib = "declare c struct point { x: i32; y: i32; }\ndeclare c struct other { a: i32; }\ndeclare c(\"geo\") function use_other(p: Ptr<point>): void;\ndeclare c(\"geo\") function use_other_t(p: Ptr<other>): void;\ndeclare c(\"geo\") function use_hidden(p: Ptr<point>): void;\ndeclare c(\"geo\") function use_lone(p: Ptr<point>): void;\n";
    project(dir.path(), header, lib);
    let o = check(dir.path(), LINUX);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for want in [
        "parameter 1 of C function `use_other` is `struct other *` in geo.h:5, not the declaration's `Ptr<point>`",
        "note: it points at C struct `other`: declare `Ptr<other>`\n",
        "parameter 1 of C function `use_other_t` is `point_t *` in geo.h:6, not the declaration's `Ptr<other>`",
        "note: it points at C struct `point_t`, declared as `point`: declare `Ptr<point>`",
        "note: it points at C type `hidden`: declare `Ptr<hidden>`, with `declare c type hidden;`",
        "note: it points at C struct `lone`: declare `Ptr<lone>`, with `declare c struct lone { … }`",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
    assert!(
        !out.contains("declare c type other") && !out.contains("declare c type point_t"),
        "{out}"
    );
}

/// An `Out<S>` is an `S *` C writes: a `const` one C only reads, and a pointer to another struct,
/// are refused, naming the parameter.
#[test]
fn an_out_struct_is_a_struct_pointer_c_writes() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "#include <stdint.h>\nstruct point { double x; double y; };\nstruct seg { int32_t n; };\nint read_point(const struct point *p);\nint get_seg(struct seg *s);\n";
    let lib = "declare c struct point { x: f64; y: f64; }\ndeclare c struct seg { n: i32; }\ndeclare c(\"geo\") function read_point(p: Out<point>): i32;\ndeclare c(\"geo\") function get_seg(s: Out<point>): i32;\n";
    project(dir.path(), header, lib);
    let o = check(dir.path(), LINUX);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for want in [
        "C only reads it: it is no out-parameter",
        "its `Out<T>`: declare",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// A `Ref<S>` is a `const S *` C reads: a non-const one C may write through, and a pointer to
/// another struct, are refused.
#[test]
fn a_ref_struct_is_a_const_struct_pointer() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "#include <stdint.h>\nstruct point { double x; double y; };\nstruct seg { int32_t n; };\nint poke_point(struct point *p);\nint read_seg(const struct seg *s);\n";
    let lib = "declare c struct point { x: f64; y: f64; }\ndeclare c struct seg { n: i32; }\ndeclare c(\"geo\") function poke_point(p: Ref<point>): i32;\ndeclare c(\"geo\") function read_seg(s: Ref<point>): i32;\n";
    project(dir.path(), header, lib);
    let o = check(dir.path(), LINUX);
    let out = output(&o);
    assert!(!o.status.success(), "{out}");
    for want in ["C may write through it", "its `Ref<S>`: declare"] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// An array of structs C fills is refused against a `const S *` (C only reads it), an array of
/// one struct against a pointer to another, and a `readonly` array of a struct with a `bool`
/// against a pointer C may write through (`S *` or `void *`), on every target's headers.
#[test]
fn an_array_of_structs_is_a_struct_pointer() {
    let Some(_zig) = zig() else { return };
    let dir = test_dir::TestDir::new();
    let header = "#include <stdint.h>\n#include <stdbool.h>\nstruct point { double x; double y; };\nstruct seg { int32_t n; };\nstruct flag { bool on; int32_t n; };\nvoid fill_points(const struct point *ps, int32_t n);\nint read_segs(const struct seg *s, int32_t n);\nvoid poke_flags(struct flag *xs, int32_t n);\nvoid poke_any(void *xs, int32_t n);\n";
    let lib = "declare c struct point { x: f64; y: f64; }\ndeclare c struct seg { n: i32; }\ndeclare c struct flag { on: bool; n: i32; }\ndeclare c(\"geo\") function fill_points(ps: point[], n: i32): void;\ndeclare c(\"geo\") function read_segs(s: readonly point[], n: i32): i32;\ndeclare c(\"geo\") function poke_flags(xs: readonly flag[], n: i32): void;\ndeclare c(\"geo\") function poke_any(xs: readonly flag[], n: i32): void;\n";
    project(dir.path(), header, lib);
    for target in [LINUX, "aarch64-apple-darwin", "x86_64-pc-windows-msvc"] {
        let o = check(dir.path(), target);
        let out = output(&o);
        assert!(!o.status.success(), "{target}:\n{out}");
        for want in [
            "C only reads the array: declare it `readonly`",
            "the array's elements are",
            "parameter 1 of C function `poke_flags`",
            "parameter 1 of C function `poke_any`",
            "`flag` has a `bool` or a non-null pointer field",
        ] {
            assert!(out.contains(want), "{target}: missing `{want}` in:\n{out}");
        }
    }
}
