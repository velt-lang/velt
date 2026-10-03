//! The parser decides where JSX starts and has the lexer re-lex from there (`lexer::relex_jsx`).
//! These inputs must stay linear: each element is re-lexed once where the parser finds it,
//! nested elements must not re-lex the rest of the file, and a re-lex drops only the lookahead
//! caches, lexer diagnostics and comments past its `<`. Work is counted (`work`), not timed, so
//! a busy machine cannot fail them: `4 * n` units must cost about 4 times the work of `n`, where
//! a quadratic regression costs about 16 times.

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
    // ... and the lexer's diagnostics and comments past it, not all of them.
    assert_linear("lexer errors before elements", 2_000, |n| {
        "const x = 1 \u{a7}; const y = <p>a</p>;\n".repeat(n)
    });
    assert_linear("comments before elements", 2_000, |n| {
        "// one\nconst y = /* two */ <p>a</p>;\n".repeat(n)
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

/// Ordinary code: functions with arithmetic, control flow, templates and a generic call.
#[test]
fn plain_code() {
    let unit = "function f(a: i64, b: i64): i64 {\n  let x = a * 2 + b / 3 - (a % 7);\n  \
                if (x > 10 && b < 3) { return x; } else { x += 1; }\n  \
                for (let i = 0; i < 10; i++) { console.log(`i=${i} x=${x}`, \"s\\n\"); }\n  \
                return g<i64>(x, [1, 2, 3], { a: 1, b }) as i64;\n}\n";
    assert_linear("plain code", 1_000, |n| unit.repeat(n));
}

/// Conditionals nested in conditionals: each level used to be parsed more than once, which is
/// exponential in the depth. The cost still grows faster than the depth (about 9 times the work
/// for 4 times the depth, an open issue), so this only rules out exponential growth: 20 levels
/// may cost at most 16 times what 5 do, where doubling per level would cost about 30 000 times.
#[test]
fn nested_conditionals() {
    let make = |n: usize| {
        let mut s = String::from("x");
        for i in 0..n {
            s = format!("c{i} ? {s} : y{i}");
        }
        format!("function f() {{ x = {s}; }}\n")
    };
    let (small, large) = (work(make(5)), work(make(20)));
    assert!(
        large <= small * 16,
        "nested conditionals: 5 levels cost {small} work, 20 levels {large}: exponential?"
    );
}

/// Parentheses nested in types, arrows and expressions, closed and unclosed (a failed
/// function-type attempt used to re-parse the inside, doubling the work per level).
#[test]
fn nested_parentheses() {
    assert_linear("nested parentheses", 5, |n| {
        let (open, close) = ("(".repeat(n), ")".repeat(n));
        [
            format!("declare async function f(path: {open}string): Promise<bool>;"),
            format!("const x: {open}i64{close} = 1;"),
            format!("declare function g(p: {open}string{close}): {open}i64{close};"),
            format!("type F = {open}(a: i64) => i64{close};"),
            format!("function h() {{ x = {open}a{close}; }}"),
            format!("function h() {{ x = (a: {open}i64{close}) => a; }}"),
            format!("function h() {{ x = {}a{close}; }}", "(a) => (".repeat(n)),
            format!("function h() {{ x = {}a{close}; }}", "(a, ".repeat(n)),
        ]
        .join("\n")
    });
}
