//! Golden-file tests: every `tests/golden/**/*.vlt` parses (except the parse-error goldens and
//! pending bug repros);
//! the parse-error golden reports at 2:16.

mod common;

use common::*;

#[test]
fn golden_m1_files_parse() {
    let files = golden_files();
    assert!(
        files.iter().any(|f| f.ends_with("hello.vlt")),
        "golden files not found"
    );
    for f in files {
        // Pending bug repros (dirs with a `.pending` marker) may be parse errors on purpose.
        if f.ancestors().any(|d| d.join(".pending").is_file()) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let (m, diags) = parse(&src);
        if matches!(
            name.as_str(),
            "parse.vlt"
                | "mut_keyword.vlt"
                | "removed_syntax.vlt"
                | "removed_question.vlt"
                | "class_field_uninferred.vlt"
                | "using_syntax.vlt"
                | "modules_export_default.vlt"
                | "strict_mode_names.vlt"
                | "generator_syntax.vlt"
                | "generator_done_undefined.vlt"
                | "private_names_syntax.vlt"
                | "quoted_property_names_reserved.vlt"
                | "type_param_defaults_required.vlt"
                | "class_expression_named.vlt"
                | "for_in_outer_variable.vlt"
                | "destructuring_computed_key_expr.vlt"
                | "overloads_missing_impl.vlt"
                | "overloads_declarations.vlt"
                | "ts_syntax_errors.vlt"
                | "ts_parse_leftovers_errors.vlt"
                | "catch_types_errors.vlt"
                | "type_operators_unsupported.vlt"
                | "param_property_pattern.vlt"
                | "param_defaults_unsupported.vlt"
                | "param_defaults_in_body_order.vlt"
                | "dynamic_import.vlt"
                | "namespace_values.vlt"
                | "overload_signature_asi_no_body.vlt"
                | "cast_union_number_literal.vlt"
                | "function_expression_errors.vlt"
                | "function_expression_this_errors.vlt"
                | "string_escape_octal.vlt"
                | "asi_restricted.vlt"
                | "computed_key_errors.vlt"
                | "destructure_computed_path.vlt"
                | "literal_method_names.vlt"
                | "define_property_accessor_this.vlt"
        ) {
            continue;
        }
        // Warnings are fine (`asi_continuation.vlt` expects some).
        assert!(
            diags.iter().all(|d| !d.is_error()),
            "{}: unexpected diagnostics: {:?}",
            f.display(),
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(!m.items.is_empty(), "{}: no items", f.display());
    }
}

#[test]
fn golden_parse_error_location() {
    let path = workspace_root().join("tests/golden/m1/errors/parse.vlt");
    let src = std::fs::read_to_string(&path).unwrap();
    let mut sm = SourceMap::new();
    let file = sm.add("parse.vlt", src.clone());
    let (m, diags) = parse_file(file, &src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    let d = &diags[0];
    assert!(d.message.contains("expected expression"), "{}", d.message);
    assert_eq!(sm.line_col(file, d.labels[0].span.lo), (2, 16));
    assert!(
        d.render(&sm)
            .starts_with("parse.vlt:2:16: error: expected expression"),
        "{}",
        d.render(&sm)
    );
    // Recovery: the function is still there.
    assert_eq!(m.items.len(), 1);
}

#[test]
fn golden_hello_shape() {
    let m = parse_ok("function main() {\n  console.log(\"Hello, Velt!\");\n}\n");
    let ItemKind::Function(f) = &m.items[0].kind else {
        panic!()
    };
    assert_eq!(f.sig.name.name, "main");
    assert!(f.sig.ret.is_none());
    let StmtKind::Expr(e) = &f.body.stmts[0].kind else {
        panic!()
    };
    assert_eq!(sx(e), "(call (. console log) [\"Hello, Velt!\"])");
}

#[test]
fn strings_golden_escapes() {
    // `strings.vlt` writes `\\` for its backslash; an unknown escape such as `\ ` is the
    // character alone, as in JS.
    let e = expr(r#""esc: \"q\" \\ \t|""#);
    assert_eq!(sx(&e), format!("{:?}", "esc: \"q\" \\ \t|"));
    let e = expr(r#""esc: \ \d|""#);
    assert_eq!(sx(&e), format!("{:?}", "esc:  d|"));
}
