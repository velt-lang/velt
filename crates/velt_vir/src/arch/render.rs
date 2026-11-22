//! The text the table generates (`cargo xtask gen-arch`): each class's instruction methods and
//! their externs, the end of its file in std/arch/x86 or std/arch/arm, and the index of C
//! intrinsics. Laid out as `velt fmt` lays them out, so the files stay formatted.

use super::{symbol_of, Lane, Row, Val, ROWS};
use crate::simd::Ext;

/// The first line of the generated part of a class file, inside the class after `isSupported`.
pub const CLASS_MARKER: &str =
    "  // The instructions, generated from velt_vir's table (crates/velt_vir/src/arch) by";

/// The widest line `velt fmt` keeps.
const WIDTH: usize = 100;

/// The generated end of `ext`'s class file: a blank line, the marker, the methods, the class's
/// closing brace and the externs. `None` when the table has no instruction of it.
pub fn class_tail(ext: Ext) -> Option<String> {
    let rows: Vec<&Row> = ROWS.iter().filter(|r| r.ext == ext).collect();
    if rows.is_empty() {
        return None;
    }
    let class = ext.arch_class();
    let mut s =
        format!("\n{CLASS_MARKER}\n  // `cargo xtask gen-arch`: edit the table, not these.\n");
    for r in &rows {
        s.push('\n');
        method(&mut s, class, ext, r);
    }
    s.push_str("}\n\n");
    // The wide types its rows take (an import may follow the code that uses it).
    let mut imported = false;
    for (width, name) in [(256, "Vector256"), (512, "Vector512")] {
        let uses = |v: Val| v.wide() == Some(width);
        if (rows.iter()).any(|r| uses(r.ret) || r.params.iter().any(|(_, v)| uses(*v))) {
            s.push_str(&format!(
                "import {{ {name} }} from \"velt/arch/x86/{}\";\n",
                name.to_lowercase()
            ));
            imported = true;
        }
    }
    if imported {
        s.push('\n');
    }
    for r in &rows {
        let params: Vec<String> = (r.params.iter())
            .map(|(n, v)| format!("{n}: {}", v.velt()))
            .collect();
        let head = format!("declare function {}", symbol_of(r));
        signature(&mut s, "", &head, &params, &format!("): {};", r.ret.velt()));
    }
    Some(s)
}

/// A method: its doc comment, signature and the call of its extern.
fn method(s: &mut String, class: &str, ext: Ext, r: &Row) {
    s.push_str("  /**\n");
    wrap(s, "   * ", r.doc);
    s.push_str("   *\n");
    let mut how = format!(
        "`{}` (`{}`). Runs only where the CPU has it: call it behind `{class}.isSupported()`, \
         or in a function gated on `{}`.",
        r.c,
        r.mnemonic(),
        ext.name()
    );
    if r.swapped() {
        how.push_str(&format!(
            " Its operands are in the opposite order to `{}`'s, which takes the indices first.",
            r.c
        ));
    }
    wrap(s, "   * ", &how);
    s.push_str("   */\n");
    let params: Vec<String> = (r.params.iter())
        .map(|(n, v)| format!("{n}: {}", v.velt()))
        .collect();
    let head = format!("  static {}", r.method);
    signature(s, "  ", &head, &params, &format!("): {} {{", r.ret.velt()));
    let args: Vec<&str> = r.params.iter().map(|(n, _)| *n).collect();
    s.push_str(&format!(
        "    return {}({});\n  }}\n",
        symbol_of(r),
        args.join(", ")
    ));
}

/// `head(params)tail` on one line, or with a parameter per line where it is too wide.
fn signature(s: &mut String, indent: &str, head: &str, params: &[String], tail: &str) {
    let one = format!("{head}({}{tail}", params.join(", "));
    if one.len() <= WIDTH {
        s.push_str(&one);
        s.push('\n');
        return;
    }
    s.push_str(&format!("{head}(\n"));
    for p in params {
        s.push_str(&format!("{indent}  {p},\n"));
    }
    s.push_str(&format!("{indent}{tail}\n"));
}

/// `text` as comment lines starting with `prefix`, at most [`WIDTH`] wide.
fn wrap(s: &mut String, prefix: &str, text: &str) {
    let mut line = prefix.to_string();
    for word in text.split(' ') {
        if line.len() > prefix.len() && line.len() + 1 + word.len() > WIDTH {
            s.push_str(line.trim_end());
            s.push('\n');
            line = prefix.to_string();
        }
        if line.len() > prefix.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    s.push_str(line.trim_end());
    s.push('\n');
}

/// docs/internals/arch-c-names.md: the velt/arch method of each C intrinsic the table has.
pub fn c_index() -> String {
    let mut rows: Vec<&Row> = ROWS.iter().collect();
    rows.sort_by_key(|r| r.c);
    let mut s = String::from(
        "# velt/arch by C intrinsic\n\n\
         Generated from velt_vir's table (`crates/velt_vir/src/arch`) by `cargo xtask gen-arch`: \
         the\nvelt/arch method for each C intrinsic of Intel's guide and Arm's ACLE that it has \
         (#3222,\n`docs/internals/design/package-machine-code.md` §3). A method runs only where \
         the CPU has its\nfeature: behind its class's `isSupported()`, or in a function gated on \
         the feature.\n\n\
         | C intrinsic | Instruction | velt/arch | Feature |\n|---|---|---|---|\n",
    );
    for r in rows {
        let note = match r.swapped() {
            true => " (operands swapped: value, then indices)",
            false => "",
        };
        s.push_str(&format!(
            "| `{}` | `{}` | `{}.{}` ({}){note} | `{}` |\n",
            r.c,
            r.mnemonic(),
            r.ext.arch_class(),
            r.method,
            r.ext.arch_module(),
            r.ext.name()
        ));
    }
    s
}

/// std/arch/x86/vector256.velt (`width` 256) or vector512.velt (512): the wide type, built from
/// and taken apart into velt/simd's 128-bit types (or, for 512 bits, two `Vector256`s), loaded
/// from and stored to arrays: plain code that runs anywhere.
pub fn vector_file(width: u32) -> String {
    let name = format!("Vector{width}");
    let mut s = format!(
        "// velt/arch/x86 (internal): `{name}<T>`, {width} bits of lanes `T` (#3222 step 5,\n\
         // docs/internals/design/package-machine-code.md §4), generated from velt_vir's table\n\
         // (crates/velt_vir/src/arch/render.rs) by `cargo xtask gen-arch`: edit the generator.\n\
         //\n\
         // Building one, taking it apart, loading and storing it is plain code that runs on any\n\
         // CPU; velt/arch/x86's AVX2 and AVX-512 methods take it as one register, behind their\n\
         // class's check.\n"
    );
    // The 128-bit types' methods (`asU8x16`, `load`, `store`), from their defining files.
    s.push_str("\nimport \"velt/simd/bytes\";\nimport \"velt/simd/floats\";\nimport \"velt/simd/words\";\n");
    if width == 512 {
        s.push_str("import { Vector256 } from \"velt/arch/x86/vector256\";\n");
    }
    let parts: &[&str] = if width == 256 {
        &["lo", "hi"]
    } else {
        &["q0", "q1", "q2", "q3"]
    };
    s.push_str(&format!(
        "\n/**\n * {width} bits of lanes `T`: the bytes of its 128-bit parts ({}), low lanes first.\n */\nexport struct {name}<T> {{\n",
        parts.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", ")
    ));
    for p in parts {
        s.push_str(&format!("  readonly {p}: u8x16;\n"));
    }
    s.push_str(&format!(
        "\n  /** The same {width} bits as lanes of type `U`. */\n  reinterpret<U>(): {name}<U> {{\n    return {{ {} }};\n  }}\n",
        parts.iter().map(|p| format!("{p}: this.{p}")).collect::<Vec<_>>().join(", ")
    ));
    if width == 512 {
        s.push_str(
            "\n  /** The vector whose low 256 bits are `lo` and high 256 bits `hi`. */\n  \
             static join<T>(lo: Vector256<T>, hi: Vector256<T>): Vector512<T> {\n    \
             return { q0: lo.lo, q1: lo.hi, q2: hi.lo, q3: hi.hi };\n  }\n\n  \
             /** Its low 256 bits. */\n  lower(): Vector256<T> {\n    \
             return { lo: this.q0, hi: this.q1 };\n  }\n\n  \
             /** Its high 256 bits. */\n  upper(): Vector256<T> {\n    \
             return { lo: this.q2, hi: this.q3 };\n  }\n",
        );
    }
    for l in Lane::ALL {
        let (t, n, x) = (l.scalar(), width / 8 / l.bytes(), l.narrow());
        let half = n / 2;
        let suffix = l.suffix();
        if width == 256 {
            s.push_str(&format!(
                "\n  /** The vector of `lo`'s lanes (0 to {}) and `hi`'s ({half} to {}). */\n  \
                 static from{}(lo: {x}, hi: {x}): Vector256<{t}> {{\n    \
                 return {{ lo: {}, hi: {} }};\n  }}\n",
                half - 1,
                n - 1,
                capitalize(&x),
                as_bytes(l, "lo"),
                as_bytes(l, "hi")
            ));
            s.push_str(&format!(
                "\n  /** The {n} lanes of `from` from `at` on. */\n  \
                 static load{suffix}(from: {t}[], at: usize = 0): Vector256<{t}> {{\n    \
                 return Vector256.from{}({x}.load(from, at), {x}.load(from, at + {half}));\n  }}\n",
                capitalize(&x)
            ));
        } else {
            s.push_str(&format!(
                "\n  /** The {n} lanes of `from` from `at` on. */\n  \
                 static load{suffix}(from: {t}[], at: usize = 0): Vector512<{t}> {{\n    \
                 return Vector512.join(Vector256.load{suffix}(from, at), Vector256.load{suffix}(from, at + {half}));\n  }}\n"
            ));
        }
    }
    s.push_str("}\n");
    for l in Lane::ALL {
        let (t, n) = (l.scalar(), width / 8 / l.bytes());
        let half = n / 2;
        s.push_str(&format!("\nextend {name}<{t}> {{\n"));
        if width == 256 {
            let x = l.narrow();
            s.push_str(&format!(
                "  /** Lanes 0 to {}. */\n  lower(): {x} {{\n    return {};\n  }}\n\n  \
                 /** Lanes {half} to {}. */\n  upper(): {x} {{\n    return {};\n  }}\n\n",
                half - 1,
                from_bytes(l, "this.lo"),
                n - 1,
                from_bytes(l, "this.hi")
            ));
        }
        s.push_str(&format!(
            "  /** Stores its {n} lanes in `to` from `at` on. */\n  store(to: {t}[], at: usize = 0) {{\n    \
             this.lower().store(to, at);\n    this.upper().store(to, at + {half});\n  }}\n}}\n"
        ));
    }
    s
}

/// `x` with its first letter in upper case (`u32x4` → `U32x4`).
fn capitalize(x: &str) -> String {
    let mut c = x.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_ascii_uppercase().to_string() + c.as_str()
    })
}

/// The bytes of `v`, a 128-bit vector of lanes `l`.
fn as_bytes(l: Lane, v: &str) -> String {
    match l {
        Lane::U8 => v.to_string(),
        _ => format!("{v}.asU8x16()"),
    }
}

/// The 128-bit vector of lanes `l` of the bytes `v`.
fn from_bytes(l: Lane, v: &str) -> String {
    match l {
        Lane::U8 => v.to_string(),
        _ => format!("{v}.as{}()", capitalize(&l.narrow())),
    }
}
