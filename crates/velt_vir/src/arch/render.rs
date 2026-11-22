//! The text the table generates (`cargo xtask gen-arch`): each class's instruction methods and
//! their externs, the end of its file in std/arch/x86 or std/arch/arm, and the index of C
//! intrinsics. Laid out as `velt fmt` lays them out, so the files stay formatted.

use super::{symbol_of, Row, ROWS};
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
    let how = format!(
        "`{}` (`{}`). Runs only where the CPU has it: call it behind `{class}.isSupported()`, \
         or in a function gated on `{}`.",
        r.c,
        r.instr,
        ext.name()
    );
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
        s.push_str(&format!(
            "| `{}` | `{}` | `{}.{}` ({}) | `{}` |\n",
            r.c,
            r.instr,
            r.ext.arch_class(),
            r.method,
            r.ext.arch_module(),
            r.ext.name()
        ));
    }
    s
}
