//! Golden-file tests: every `tests/golden/**/*.velt` parses (except the parse-error goldens and
//! pending bug repros);
//! the parse-error golden reports at 2:16.

mod common;

use common::*;

#[test]
fn golden_m1_files_parse() {
    let files = golden_files();
    assert!(
        files.iter().any(|f| f.ends_with("hello.velt")),
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
            "parse.velt"
                | "mut_keyword.velt"
                | "removed_syntax.velt"
                | "removed_question.velt"
                | "class_field_uninferred.velt"
                | "using_syntax.velt"
                | "_default_export_twice_lib.velt"
                | "default_name_reserved.velt"
                | "import_type_default_and_named.velt"
                | "strict_mode_names.velt"
                | "generator_syntax.velt"
                | "generator_done_undefined.velt"
                | "private_names_syntax.velt"
                | "quoted_property_names_reserved.velt"
                | "type_param_defaults_required.velt"
                | "class_expression_named.velt"
                | "for_in_outer_variable.velt"
                | "destructuring_computed_key_expr.velt"
                | "overloads_missing_impl.velt"
                | "overloads_declarations.velt"
                | "ts_syntax_errors.velt"
                | "ts_parse_leftovers_errors.velt"
                | "catch_types_errors.velt"
                | "type_operators_unsupported.velt"
                | "param_property_pattern.velt"
                | "param_defaults_unsupported.velt"
                | "param_defaults_in_body_order.velt"
                | "dynamic_import.velt"
                | "namespace_values.velt"
                | "overload_signature_asi_no_body.velt"
                | "cast_union_number_literal.velt"
                | "function_expression_errors.velt"
                | "function_expression_this_errors.velt"
                | "string_escape_octal.velt"
                | "asi_restricted.velt"
                | "computed_key_errors.velt"
                | "destructure_computed_path.velt"
                | "literal_method_names.velt"
                | "define_property_accessor_this.velt"
                | "static_block_return.velt"
                | "global_this_declare_global.velt"
                | "global_this_declare_let.velt"
                | "jsx_tag_declared_no_value.velt"
                | "_global_this_declared_lib.velt"
                | "abstract_class_syntax.velt"
                | "operator_index_syntax.velt"
                | "decorators_placement.velt"
                | "decorators_tc39_forms.velt"
                | "decorators_default_placement.velt"
                | "decorators_default_expression.velt"
                | "decorators_class_binding.velt"
                | "jsx_table_module_namespace.velt"
        ) {
            continue;
        }
        // Warnings are fine (`asi_continuation.velt` expects some).
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
    let path = workspace_root().join("tests/golden/m1/errors/parse.velt");
    let src = std::fs::read_to_string(&path).unwrap();
    let mut sm = SourceMap::new();
    let file = sm.add("parse.velt", src.clone());
    let (m, diags) = parse_file(file, &src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    let d = &diags[0];
    assert!(d.message.contains("expected expression"), "{}", d.message);
    assert_eq!(sm.line_col(file, d.labels[0].span.lo), (2, 16));
    assert!(
        d.render(&sm)
            .starts_with("parse.velt:2:16: error: expected expression"),
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
    // `strings.velt` writes `\\` for its backslash; an unknown escape such as `\ ` is the
    // character alone, as in JS.
    let e = expr(r#""esc: \"q\" \\ \t|""#);
    assert_eq!(sx(&e), format!("{:?}", "esc: \"q\" \\ \t|"));
    let e = expr(r#""esc: \ \d|""#);
    assert_eq!(sx(&e), format!("{:?}", "esc:  d|"));
}
