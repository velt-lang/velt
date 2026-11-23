//! The layout decorators of value types (`velt/compiler`, #3222 step 6,
//! docs/internals/design/package-machine-code.md §5):
//!
//! - `@repr("transparent")` passes a struct of one field exactly as the field: a `declare c`
//!   function and an `@export` one cross the field's C type on every C calling convention Velt
//!   targets, which is what clang gives the field's C type, and not what it gives a C struct of
//!   it (Windows x64 passes `struct { double }` in an integer register).
//! - `@align(n)` aligns a struct's values and pads its size, in arrays and in other structs, and
//!   heap blocks of them come aligned.
//! - What breaks their rules is an error.
//!
//! The values crossing at run time (in debug and release builds): std/compiler/layout.test.velt.

use std::path::Path;
use std::process::{Command, Output};

mod no_window;
mod test_dir;

/// The C calling conventions Velt targets: SysV x86-64, AAPCS64, Darwin arm64, Windows x64.
const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

/// A package `cabi` whose code is `lib` (allowed to call the C library unchecked), and a program
/// calling its `all()`.
fn project(dir: &Path, lib: &str) {
    let (pkg, app) = (dir.join("cabi"), dir.join("app"));
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cabi\", version: \"0.1.0\", native: { c: { c: { unchecked: true } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cabi\"],\n  dependencies: { cabi: { path: \"../cabi\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/main.velt"),
        "import { all } from \"cabi\";\n\nfunction main() {\n  console.log(all());\n}\n",
    )
    .unwrap();
}

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// The program's release LLVM IR for `target`.
fn llvm(dir: &Path, target: &str) -> String {
    let o = velt(
        &dir.join("app"),
        &["build", "--release", "--emit", "llvm", "--target", target],
    );
    assert!(
        o.status.success(),
        "{target}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// C functions taking and returning each kind of transparent struct, and an `@export` function of
/// one; `T` is replaced with the struct (`D`) or with its field's type (`f64`), `V` with
/// `{ v: x }` or `x`, and `.v` read off a result where there are structs.
const LIB: &str = r#"import { Ptr, export as cExport } from "velt/ffi";
import { repr } from "velt/compiler";

@repr("transparent")
export struct D { readonly v: f64; }
@repr("transparent")
export struct F { readonly v: f32; }
@repr("transparent")
export struct Q { readonly v: u64; }
@repr("transparent")
export struct B { readonly v: u8; }
@repr("transparent")
export struct P { readonly v: Ptr<u8>; }

declare c("c") function xt_d(x: TD): TD;
declare c("c") function xt_f(x: TF): TF;
declare c("c") function xt_q(x: TQ): TQ;
declare c("c") function xt_b(x: TB): TB;
declare c("c") function xt_p(x: TP): TP;

@cExport("xe_b")
function echoB(x: TB): TB {
  return x;
}

@cExport("xe_d")
function echoD(x: TD): TD {
  return x;
}

export function all(): f64 {
  // SAFETY: the address is never dereferenced, and nothing here runs: the test reads the IR.
  const p = unsafe { Ptr.at<u8>(8) };
  // SAFETY: as above.
  return unsafe {
    xt_d(V(1))R + (xt_f(V(2))R as f64) + (xt_q(V(3))R as f64) + (xt_b(V(4))R as f64) + (Ptr.addressOf(xt_p(V(p))R) as f64)
  };
}
"#;

/// [`LIB`] with transparent structs (`structs`) or with their fields' types.
fn lib(structs: bool) -> String {
    let mut s = LIB.to_string();
    for (t, field) in [
        ("TD", "f64"),
        ("TF", "f32"),
        ("TQ", "u64"),
        ("TB", "u8"),
        ("TP", "Ptr<u8>"),
    ] {
        s = s.replace(t, if structs { &t[1..] } else { field });
    }
    let (open, close, read) = match structs {
        true => ("{ v: ", " }", ".v"),
        false => ("", "", ""),
    };
    // `V(x)` → `{ v: x }` or `x`; `)R` → `).v` or `)`.
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(at) = rest.find("V(") {
        out.push_str(&rest[..at]);
        let inner = &rest[at + 2..];
        let end = inner.find(')').unwrap();
        out.push_str(&format!("{open}{}{close}", &inner[..end]));
        rest = &inner[end + 1..];
    }
    out.push_str(rest);
    out.replace(")R", &format!("){read}"))
}

/// The lines of `ir` declaring or defining the C functions and exports (`xt_*`, `xe_*`).
fn c_lines(ir: &str) -> Vec<String> {
    let mut lines: Vec<String> = ir
        .lines()
        .filter(|l| l.starts_with("declare") || l.starts_with("define"))
        .filter(|l| l.contains("@\"xt_") || l.contains("@\"xe_"))
        .map(|l| {
            l.split(" #")
                .next()
                .unwrap_or(l)
                .trim_end_matches(" {")
                .to_string()
        })
        .collect();
    lines.sort();
    lines
}

/// `declare zeroext i8 @"xt_b"(i8 zeroext)` → `xt_b: i8 (i8)`: the types alone, which is what
/// clang's declaration of the C prototype is compared by (the extension attributes are the
/// scalar C ABI's, which tests/c_export.rs and tests/c_functions.rs hold to clang's).
fn types_of(line: &str) -> String {
    let words = |s: &str| -> String {
        s.split([' ', ','])
            .filter(|w| {
                !w.is_empty()
                    && !matches!(
                        *w,
                        "declare"
                            | "define"
                            | "dso_local"
                            | "noundef"
                            | "zeroext"
                            | "signext"
                            | "local_unnamed_addr"
                            | "dllexport"
                            | "returned"
                    )
                    && !w.starts_with('%')
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (head, params) = line.split_once('(').unwrap();
    // Up to the closing parenthesis: clang writes function attributes after it.
    let params = params.rsplit_once(')').map_or(params, |(p, _)| p);
    let (ret, name) = head.rsplit_once(" @").unwrap();
    let name = name.trim_matches('"');
    let params: Vec<String> = params.split(',').map(words).collect();
    format!("{name}: {} ({})", words(ret), params.join(", "))
}

/// The declarations clang makes for `target` of C prototypes taking and returning each field's
/// C type, and of the exports' definitions, by [`types_of`]; with `structs`, of C structs of one
/// such field instead.
fn clang_lines(clang: &Path, dir: &Path, target: &str, structs: bool) -> Vec<String> {
    let source = match structs {
        false => "#include <stdint.h>\ndouble xt_d(double); float xt_f(float); uint64_t xt_q(uint64_t); uint8_t xt_b(uint8_t); uint8_t *xt_p(uint8_t *);\nuint8_t xe_b(uint8_t x) { return x; }\ndouble xe_d(double x) { return x; }\n",
        true => "#include <stdint.h>\nstruct D { double v; };\nstruct D xt_d(struct D);\nstruct D xe_d(struct D x) { return x; }\n",
    };
    let use_all = match structs {
        false => "double use(void) { return xt_d(1) + xt_f(2) + xt_q(3) + xt_b(4) + (double)(uintptr_t)xt_p(0); }\n",
        true => "double use(void) { struct D d = { 1 }; return xt_d(d).v; }\n",
    };
    let c = dir.join(format!("{target}-{structs}.c"));
    std::fs::write(&c, format!("{source}{use_all}")).unwrap();
    let o = Command::new(clang)
        .args([
            "-target",
            target,
            "-O1",
            "-S",
            "-emit-llvm",
            "-ffreestanding",
            "-o",
            "-",
        ])
        .arg(&c)
        .output()
        .expect("clang runs");
    assert!(
        o.status.success(),
        "clang {target}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let ir = String::from_utf8_lossy(&o.stdout);
    let mut lines: Vec<String> = ir
        .lines()
        .filter(|l| l.starts_with("declare") || l.starts_with("define"))
        .filter(|l| l.contains("@xt_") || l.contains("@xe_"))
        .map(|l| {
            let l = l.split(" #").next().unwrap_or(l).trim_end_matches(" {");
            types_of(&l.replace("@xt_", "@\"xt_").replace("@xe_", "@\"xe_"))
        })
        .collect();
    lines.sort();
    lines
}

/// A transparent struct crosses to C exactly as its field on every target: Velt's declarations
/// and exports with the structs are those with the fields' types, and clang declares the fields'
/// C prototypes with the same types. A C struct of the same field is another thing: on Windows
/// x64 an `i64`, which is why the decorator is a promise of its own.
#[test]
fn a_transparent_struct_crosses_to_c_as_its_field_on_every_target() {
    let dir = test_dir::TestDir::new();
    let (with, without) = (dir.path().join("with"), dir.path().join("without"));
    project(&with, &lib(true));
    project(&without, &lib(false));
    let clang = velt_codegen_llvm::find_clang();
    if clang.is_none() {
        eprintln!("note: clang not available; comparing Velt's two programs only");
    }
    for target in TARGETS {
        let ours = c_lines(&llvm(&with, target));
        assert_eq!(ours.len(), 7, "{target}: {ours:#?}");
        assert_eq!(ours, c_lines(&llvm(&without, target)), "{target}");
        let Some(clang) = &clang else {
            continue;
        };
        let mut typed: Vec<String> = ours.iter().map(|l| types_of(l)).collect();
        typed.sort();
        let theirs = clang_lines(clang, dir.path(), target, false);
        assert_eq!(typed, theirs, "{target}: Velt's against clang's");
        // Windows x64 passes a C struct of one `double` in an integer register.
        if target == "x86_64-pc-windows-msvc" {
            let as_struct = clang_lines(clang, dir.path(), target, true);
            assert!(
                as_struct.contains(&"xt_d: i64 (i64)".to_string()),
                "{target}: a C struct of one double is {as_struct:?}"
            );
        }
    }
}

/// The layout of aggregate `name` in `vir`, after its id.
fn layout(vir: &str, name: &str) -> String {
    vir.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no aggregate `{name}` in:\n{vir}"))
        .split_once(' ')
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default()
}

const ALIGNED: &str = r#"import { align, repr } from "velt/compiler";

/** A counter per cache line. */
@align(64)
struct Line {
  n: u64;
}

/** Holding an aligned struct: takes its alignment. */
struct Tagged {
  tag: u8;
  line: Line;
}

/** `@align` never lowers the natural alignment. */
@align(2)
struct Word {
  w: u64;
}

/** Below a cache line: its arrays' buffers are aligned by `@align` alone. */
@align(32)
struct Pair {
  a: u32;
  b: u8;
}

@repr("transparent")
struct Meters {
  m: f64;
}

function main() {
  const lines: Line[] = [{ n: 1 }, { n: 2 }];
  lines.push({ n: 3 });
  const tagged: Tagged[] = [{ tag: 1, line: { n: 4 } }];
  const words: Word[] = [{ w: 5 }];
  const pairs: Pair[] = [{ a: 6, b: 7 }];
  const m: Meters = { m: 8 };
  // Captured and changed by a closure: a counted box, whose payload is aligned too.
  let boxed: Line = { n: 8 };
  const bump = () => {
    boxed = { n: boxed.n + 1 };
  };
  // Stored, so it outlives the call and `boxed` lives in a box rather than on the stack.
  const later: (() => void)[] = [bump];
  later[0]();
  console.log(lines.length, lines[2].n, tagged[0].line.n, words[0].w, pairs[0].b, m.m, boxed.n);
}
"#;

/// `@align(n)` raises a struct's alignment to `n` and pads its size to a multiple of it; a
/// struct holding one takes its alignment; a smaller `n` changes nothing; a transparent struct
/// has its field's layout. An array's buffer and a counted box come from the allocator with the
/// elements' alignment (a box's payload after a header as long as that alignment), and the
/// program prints its fields as written in both builds.
#[test]
fn align_raises_a_struct_s_alignment_and_pads_its_size() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), ALIGNED).unwrap();
    let o = velt(dir.path(), &["build", "main.velt", "--emit", "vir"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let vir = String::from_utf8_lossy(&o.stdout).to_string();
    assert_eq!(layout(&vir, "Line"), "Line size=64 align=64 { u64@0 }");
    let tagged = layout(&vir, "Tagged");
    assert!(
        tagged.starts_with("Tagged size=128 align=64 { u8@0, agg#") && tagged.ends_with("@64 }"),
        "{tagged}"
    );
    assert_eq!(layout(&vir, "Word"), "Word size=8 align=8 { u64@0 }");
    assert_eq!(
        layout(&vir, "Pair"),
        "Pair size=32 align=32 { u32@0, u8@4 }"
    );
    // `pairs`' buffer: a heap block aligned to 32.
    assert!(
        (vir.lines()).any(|l| l.contains("velt_rt_alloc(") && l.contains(", 32_u64)")),
        "no 32-byte aligned block in:\n{vir}"
    );
    // `boxed`'s box: a 64-byte header (the count in its last word) before the 64-byte payload,
    // so the payload is aligned to 64 as the block is; freed with the same size and alignment.
    for call in ["velt_rt_alloc(128_u64, 64_u64)", "velt_rt_free("] {
        assert!(vir.contains(call), "no `{call}` in:\n{vir}");
    }
    assert!(
        (vir.lines()).any(|l| l.contains("velt_rt_free(") && l.contains(", 128_u64, 64_u64)")),
        "the box is not freed as it was allocated:\n{vir}"
    );
    assert_eq!(layout(&vir, "Meters"), "Meters size=8 align=8 { f64@0 }");
    for release in [false, true] {
        let mut args = vec!["run", "main.velt"];
        if release {
            args.push("--release");
        }
        let o = velt(dir.path(), &args);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "3 3 4 5 7 8 9\n",
            "release: {release}"
        );
    }
}

/// What each decorator can't be on, and the rules of its argument and of a transparent struct.
#[test]
fn the_layout_decorators_rules_are_errors() {
    let dir = test_dir::TestDir::new();
    let src = r#"import { align, repr } from "velt/compiler";

@repr("transparent")
class Box { v: f64 = 0; }

@align(16)
enum Color { Red, Green }

@repr("transparent")
declare c struct CPoint { x: f64; }

@repr("transparent")
struct Two { a: f64; b: f64; }

@repr("transparent")
struct Maybe { a?: f64; }

@repr("transparent")
struct Nothing { v: void; }

@repr("transparent")
struct Empty {}

@align(3)
struct Three { a: u8; }

@align(8192)
struct Huge { a: u8; }

@align(0)
struct Zero { a: u8; }

@repr("transparent")
@align(16)
struct Both { a: f64; }

@repr("opaque")
struct Wrong { a: f64; }

function main() {}
"#;
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let o = velt(dir.path(), &["check", "main.velt"]);
    let out = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    for want in [
        "`@repr` applies to a struct, not to a class",
        "`@align` applies to a struct, not to an enum",
        "`@repr` applies to a struct, not to a `declare c struct`",
        "`@repr(\"transparent\")` struct `Two` has exactly one field: it has 2 fields",
        "`@repr(\"transparent\")` struct `Maybe` has exactly one field: its field is optional",
        "`@repr(\"transparent\")` struct `Nothing` has exactly one field: its field is `void`",
        "`@repr(\"transparent\")` struct `Empty` has exactly one field: it has no field",
        "`@align(3)` is not a power of two from 1 to 4096",
        "`@align(8192)` is not a power of two from 1 to 4096",
        "`@align(0)` is not a power of two from 1 to 4096",
        "a `@repr(\"transparent\")` struct has its field's layout, which `@align` would change",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}

/// A struct that breaks the transparent rules doesn't cross to C as anything: the C function's
/// declaration is an error as for any other struct.
#[test]
fn a_struct_that_is_not_transparent_does_not_cross_to_c() {
    let dir = test_dir::TestDir::new();
    let lib = "import { repr } from \"velt/compiler\";\n\n@repr(\"transparent\")\nexport struct Two { readonly a: f64; readonly b: f64; }\nexport struct Plain { readonly a: f64; }\n\ndeclare c(\"c\") function two(x: Two): f64;\ndeclare c(\"c\") function plain(x: Plain): f64;\n\nexport function all(): f64 {\n  return 0;\n}\n";
    project(dir.path(), lib);
    let o = velt(&dir.path().join("app"), &["check"]);
    let out = String::from_utf8_lossy(&o.stderr).to_string() + &String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    for want in [
        "parameter `x` of C function `two` has type `Two`, which cannot cross to C",
        "parameter `x` of C function `plain` has type `Plain`, which cannot cross to C",
        "`@repr(\"transparent\")` struct `Two` has exactly one field: it has 2 fields",
    ] {
        assert!(out.contains(want), "missing `{want}` in:\n{out}");
    }
}
