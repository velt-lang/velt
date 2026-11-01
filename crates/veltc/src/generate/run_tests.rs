//! The host's records: only those starting with the run's marker count.

use super::*;
use velt_sema::generated::GenFunction;

fn read(marker: &str, out: &str) -> Result<Vec<(String, Written)>, String> {
    let output = HostOutput {
        marker: marker.into(),
        out: out.as_bytes().to_vec(),
    };
    let mut written = vec![("i1".to_string(), Written::default())];
    let mut errors = vec![];
    // One generated function, `hello` of generator `g0`; a model without types.
    let demand = Demand {
        functions: vec![GenFunction {
            module: "main".into(),
            function: "hello".into(),
            generator: "./gen".into(),
            generator_span: Span::DUMMY,
            params: vec![],
            ret: "string".into(),
            is_async: false,
            throws: None,
        }],
        ..Demand::default()
    };
    let fn_keys = ["g0".to_string()];
    let read = parse_frames(
        &output,
        (&mut written, &mut errors),
        (&demand, &fn_keys),
        Span::DUMMY,
    );
    read?;
    assert!(errors.is_empty());
    Ok(written)
}

/// What a generator prints is skipped, also a line that looks like a record (`#…`), one that
/// would have forged a body under the old format, and text with no newline before a record.
#[test]
fn printed_text_is_not_a_record() {
    let m = "0123456789abcdef0123456789abcdef";
    let out = format!(
        "# a heading\n#body i1 9\nforged();\n#done 0\n\npartial#{m} body i1 7\nreal();\n\n#{m} done 0\n\n"
    );
    let written = read(m, &out).expect("the records");
    assert_eq!(written[0].1.body.as_deref(), Some("real();"));
}

/// A record's text may hold anything, the marker included: it is read by its length.
#[test]
fn a_record_is_read_by_its_length() {
    let m = "feedfeedfeedfeedfeedfeedfeedfeed";
    let body = format!("a\n#{m} done 0\nb");
    let out = format!("\n#{m} body i1 {}\n{body}\n\n#{m} done 0\n\n", body.len());
    let written = read(m, &out).expect("the records");
    assert_eq!(written[0].1.body.as_deref(), Some(body.as_str()));
}

/// Without the marker's records, the output is incomplete, whatever it printed.
#[test]
fn records_without_the_marker_are_ignored() {
    let out = "#body i1 2\nx;\n#done 0\n\n";
    let err = read("00000000000000000000000000000000", out).expect_err("no records");
    assert!(err.contains("stopped before it finished"), "{err}");
}

#[test]
fn markers_differ() {
    assert_ne!(new_marker(), new_marker());
    assert_eq!(new_marker().len(), 32);
}

/// A length past the end of the output (or of `usize`) is an error, not a crash.
#[test]
fn a_length_past_the_output_is_an_error() {
    let m = "abababababababababababababababab";
    for len in ["18446744073709551615", "99"] {
        let out = format!("\n#{m} body i1 {len}\nx\n");
        let err = read(m, &out).expect_err("a bad length");
        assert!(
            err.contains("longer than the output") || err.contains("shorter"),
            "{err}"
        );
    }
}

/// An instance of anything but a generated function of the same generator, or at a type the
/// model does not have, is refused.
#[test]
fn only_generated_functions_get_instances() {
    let m = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
    let rec = |header: &str| format!("\n#{m} {header} 0\n\n#{m} done 0\n\n");
    assert!(read(m, &rec("instance n1 g0 hello")).is_ok());
    for (header, want) in [
        ("instance n1 g0 nope", "not a generated function"),
        ("instance n1 g1 hello", "not a generated function"),
        ("instance n1 g0 hello 0", "unknown type"),
        ("instance n1 g0 hello -1", "invalid digit"),
    ] {
        let err = read(m, &rec(header)).expect_err(header);
        assert!(err.contains(want), "{header}: {err}");
    }
}
