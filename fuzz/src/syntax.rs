//! Parser and formatter properties.

use velt_common::FileId;

/// No panics on any input; if `src` parses cleanly, `velt fmt` output parses to the same AST
/// (spans aside) and formatting it again changes nothing.
pub fn check(src: &str) {
    let (module, diags) = velt_syntax::parse_file(FileId(0), src);
    if diags.iter().any(|d| d.is_error()) {
        return;
    }
    let formatted = velt_fmt::format_source(src)
        .unwrap_or_else(|_| panic!("formatter rejected a file the parser accepts:\n{src}"));
    let (reparsed, diags) = velt_syntax::parse_file(FileId(0), &formatted);
    assert!(
        !diags.iter().any(|d| d.is_error()),
        "formatted output does not parse:\n--- input\n{src}\n--- formatted\n{formatted}"
    );
    assert_eq!(
        shape(&velt_syntax::dump(&module)),
        shape(&velt_syntax::dump(&reparsed)),
        "formatting changed the AST:\n--- input\n{src}\n--- formatted\n{formatted}"
    );
    let again = velt_fmt::format_source(&formatted).expect("formatted output parses");
    assert_eq!(
        formatted, again,
        "formatting is not idempotent:\n--- input\n{src}"
    );
}

/// The AST dump without source positions (the `lo`/`hi` lines of every `Span`).
fn shape(dump: &str) -> String {
    dump.lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("lo: ") || t.starts_with("hi: "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_valid_and_invalid_programs() {
        super::check(
            "function main() {\n  const x = [1, 2].map((v) => v * 2);\n  console.log(x);\n}\n",
        );
        super::check("function main( {");
        super::check("");
    }
}
