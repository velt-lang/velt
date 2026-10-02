//! The parser decides where JSX starts and has the lexer re-lex from there (`lexer::relex_jsx`).
//! These inputs must stay linear: each element is re-lexed once where the parser finds it,
//! nested elements must not re-lex the rest of the file, and a re-lex drops only the lookahead
//! caches and lexer diagnostics past its `<`. Work is counted (`work`), not timed, so a busy
//! machine cannot fail them: `4 * n` units must cost about 4 times the work of `n`, where a
//! quadratic regression costs about 16 times.

use velt_common::FileId;

/// Work for parsing `src`, on a thread with the parser's stack (as `parse_file` runs it).
fn work(src: String) -> u64 {
    std::thread::Builder::new()
        .stack_size(crate::PARSER_STACK_BYTES)
        .spawn(move || {
            crate::work::take();
            let _ = crate::parse_with_comments(FileId(0), &src);
            crate::work::take()
        })
        .expect("ICE: cannot spawn a test thread")
        .join()
        .expect("ICE: the parser panicked")
}

fn assert_linear(what: &str, n: usize, make: impl Fn(usize) -> String) {
    let (small, large) = (work(make(n)), work(make(4 * n)));
    assert!(
        large < small * 6,
        "{what}: {n} units cost {small} work, {} units {large}: not linear",
        4 * n
    );
}

#[test]
fn nested_elements_and_generics() {
    let unit = "const a = <ul class=\"x\">{xs.map((i) => <li key={i}>it's {i}</li>)}</ul>;\n\
                const id = <T>(x: T): T => x;\nconst u = v.as<User>();\n";
    assert_linear("nested elements and generics", 1_000, |n| unit.repeat(n));
}

#[test]
fn caches_past_a_relex_only() {
    // Each re-lex drops the lookahead caches past its `<`, not the ones for the whole file.
    assert_linear("parentheses before elements", 1_000, |n| {
        let parens = "function f(a: i64): i64 { return ((a + (1)) * (a - (2))) / (a + (3)); }\n";
        parens.repeat(n) + &"const p = <p>{(1)}</p>;\n".repeat(n)
    });
    // ... and the lexer's diagnostics past it, not all of them.
    assert_linear("lexer errors before elements", 2_000, |n| {
        "const x = 1 \u{a7}; const y = <p>a</p>;\n".repeat(n)
    });
}

#[test]
fn failed_attempts_and_unclosed_elements() {
    // Generic arrow attempts that fail late, and unclosed elements, stay linear too.
    assert_linear("unclosed generic arrows", 5_000, |n| {
        format!("x = {};", "<a>(".repeat(n))
    });
    assert_linear("unclosed elements in braces", 5_000, |n| {
        format!("x = {};", "{<a>(x".repeat(n))
    });
    assert_linear("unfinished generic arrow parameters", 5_000, |n| {
        format!("x = {};", "<T>(x: T".repeat(n))
    });
}
