//! Numbers as text in std's Velt code (#1982): `tests/golden/std/number_format.velt` checks that
//! the output stays Node's; this checks the release build's shape: no call into the runtime's
//! formatters, and the float path is one function marked `@noInline` (#1884) that every `${x}`
//! calls instead of a copy at each site.

use std::path::Path;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The release VIR of a golden program.
fn release_vir(golden: &str) -> String {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(golden);
    let opts = BuildOptions {
        input,
        release: true,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

#[test]
fn numbers_print_without_the_runtime_and_floats_through_one_function() {
    let vir = release_vir("std/number_format.velt");
    // The bodies of the functions that may run often: cold functions are left as they are.
    // (The runtime prints no integer for a template or `String()` at all: its integer
    // formatters were deleted, #2197.)
    let mut hot = vec![];
    let mut keep = false;
    for line in vir.lines() {
        if line.starts_with("fn#") {
            keep = !line.contains(" cold");
        } else if keep {
            hot.push(line);
        }
    }
    // Not `velt_rt_strbuf_push_f64`: `StringBuilder.appendNumber` and the JSON writer (the
    // golden's builder and JSON section, inlined into `main`) send a float that is not a whole
    // number to the runtime's printer by design, since std's float printer is not loaded for
    // them. Templates and `String()` take std's: the float formatter is checked below.
    let runtime = "velt_rt_str_from_f64";
    assert!(
        !hot.iter().any(|l| l.contains(runtime)),
        "a call of `{runtime}`\n{vir}"
    );
    let float_fns: Vec<&str> = vir
        .lines()
        .filter(|l| l.starts_with("fn#") && (l.contains("N7f64Text") || l.contains("N7pushF64")))
        .collect();
    assert!(!float_fns.is_empty(), "no float formatter\n{vir}");
    for header in float_fns {
        assert!(header.contains(" noinline"), "inlinable: {header}");
    }
}

#[test]
fn a_builder_param_that_is_appended_to_is_not_readonly() {
    // `pushF64(buf, x)` appends to `buf` through `strbufPushAsciiWords`, whose `buf` is
    // `Mut<string>`: the parameter is written, so it must not be `readonly` (LLVM would let the
    // caller keep the builder's length from before the call) or `frozen`.
    let vir = release_vir("std/number_format.velt");
    let mut lines = vir.lines();
    let mut seen = 0;
    while let Some(line) = lines.next() {
        if line.starts_with("fn#") && line.contains("N7pushF64(") {
            let param = lines.next().unwrap_or_default();
            assert!(param.contains("// buf"), "{line}\n{param}");
            assert!(
                !param.contains("readonly") && !param.contains("frozen"),
                "{line}\n{param}"
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "no pushF64\n{vir}");
}

#[test]
fn a_float_no_syntax_marks_takes_std_printer() {
    // `tests/golden/lang/float_text_miss.velt`: its floats reach `${x}` and `String(x)` in a module
    // whose syntax marks none of them as floats. Sema sees their types and demands std's float
    // printer during the check (velt/lang/hooks/number_float, #2161 step 3), so the conversions
    // call it, and no template or `String()` of the program goes to the runtime's printer.
    let vir = release_vir("lang/float_text_miss.velt");
    let header = |l: &&str| l.starts_with("fn#");
    assert!(
        vir.lines()
            .filter(header)
            .any(|l| l.contains("N7f64Text") || l.contains("N7pushF64")),
        "std's float printer is not compiled in\n{vir}"
    );
    assert!(
        !vir.lines()
            .any(|l| !l.starts_with("fn#") && l.contains("velt_rt_str_from_f64")),
        "a call of the runtime's printer\n{vir}"
    );
}

#[test]
fn std_printer_is_left_out_of_a_program_that_converts_no_float() {
    // An integer in a template demands the integer hooks, not the float ones: no float printer in
    // the program, whichever way it is loaded.
    let vir = release_vir("m1/hello.velt");
    assert!(
        !vir.lines()
            .any(|l| l.starts_with("fn#") && (l.contains("N7f64Text") || l.contains("N7pushF64"))),
        "{vir}"
    );
}
