//! Robustness: no panics on truncated/garbage input, bounded nesting depth, linear-time
//! ternary disambiguation, and parse speed. Parse cost growth is counted in `src/linear_tests.rs`.

mod common;

use common::*;

#[test]
fn kitchen_sink_parses_cleanly() {
    let m = parse_ok(KITCHEN_SINK);
    assert!(m.items.len() >= 15);
    let dumped = dump(&m);
    assert!(dumped.contains("Module"));
}

/// Every prefix of every golden file and of the kitchen sink must parse without panicking.
///
/// Quadratic in the file sizes, so the work is cut into shards, one test each: a parallel test
/// runner runs them as separate processes (threads in one process mostly wait on each other in
/// the kernel, allocating and freeing the parser's large buffers).
fn prefixes_never_panic(shard: usize) {
    let mut sources: Vec<String> = golden_files()
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect();
    sources.push(KITCHEN_SINK.to_string());
    const CHUNK: usize = 64;
    let mut k = 0;
    for src in &sources {
        let positions: Vec<usize> = src.char_indices().map(|(i, _)| i).collect();
        for chunk in positions.chunks(CHUNK) {
            k += 1;
            if k % PREFIX_SHARDS != shard {
                continue;
            }
            for &i in chunk {
                let _ = parse(&src[..i]);
                let _ = parse(&src[i..]);
            }
        }
    }
}

const PREFIX_SHARDS: usize = 8;

macro_rules! prefix_shards {
    ($($name:ident = $shard:literal),*) => {
        $(
            #[test]
            fn $name() {
                prefixes_never_panic($shard);
            }
        )*
    };
}

prefix_shards!(
    prefixes_never_panic_0 = 0,
    prefixes_never_panic_1 = 1,
    prefixes_never_panic_2 = 2,
    prefixes_never_panic_3 = 3,
    prefixes_never_panic_4 = 4,
    prefixes_never_panic_5 = 5,
    prefixes_never_panic_6 = 6,
    prefixes_never_panic_7 = 7
);

#[test]
fn garbage_never_panics() {
    // Deterministic xorshift PRNG.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    const PIECES: &[&str] = &[
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        ">>",
        "=",
        "=>",
        "?",
        "?.",
        ":",
        ";",
        ",",
        ".",
        "...",
        "..=",
        "`",
        "${",
        "\"",
        "'",
        "\\",
        "/*",
        "//",
        "\n",
        " ",
        "a",
        "b",
        "1",
        "0x",
        "1e",
        "_",
        "let",
        "const",
        "function",
        "class",
        "struct",
        "match",
        "if",
        "else",
        "for",
        "of",
        "as",
        "new",
        "async",
        "await",
        "return",
        "enum",
        "interface",
        "type",
        "import",
        "export",
        "from",
        "static",
        "|",
        "&",
        "-",
        "+",
        "*",
        "**",
        "!",
        "é",
        "€",
        "\u{1F600}",
        "\0",
        "\r",
        "#",
        "@",
        "<div>",
        "</div>",
        "<>",
        "</>",
        "/>",
        "<a ",
        "b=\"x\"",
        "{...",
        "&amp;",
        "&#x",
        "<T,>",
        "<T>",
        "<T>(",
        ") =>",
        ".as<",
        "{<",
        "</",
        "x:y",
    ];
    for _ in 0..3000 {
        let len = (next() % 60) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push_str(PIECES[(next() % PIECES.len() as u64) as usize]);
        }
        let _ = parse(&s);
    }
    // Raw random bytes (lossily converted to UTF-8).
    for _ in 0..500 {
        let len = (next() % 200) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        let _ = parse(&String::from_utf8_lossy(&bytes));
    }
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    for (open, close) in [
        ("(", ")"),
        ("[", "]"),
        ("{a: ", "}"),
        ("-", ""),
        ("!", ""),
        ("a = ", ""),
        ("2 ** ", ""),
        ("c ? 1 : ", ""),
        ("x => ", ""),
        ("`${", "}`"),
        ("f(", ")"),
        ("<a>", "</a>"),
        ("<a b=", " />"),
    ] {
        let src = format!(
            "function f() {{ x = {}1{}; }}",
            open.repeat(5000),
            close.repeat(5000)
        );
        let errs = errors(&src);
        assert!(
            errs.iter().any(|e| e.contains("nested too deeply")),
            "{}: {:?}",
            open,
            errs
        );
    }
    let src = format!("function f() {}{}", "{ ".repeat(5000), "} ".repeat(5000));
    assert!(!errors(&src).is_empty());
    let src = format!("function f() {{ {} x(); }}", "l: ".repeat(5000));
    assert!(!errors(&src).is_empty());
    let src = format!(
        "function f() {{ if (a) x(); {} }}",
        "else if (b) y(); ".repeat(5000)
    );
    assert!(!errors(&src).is_empty());
    let src = format!(
        "function f() {{ match (x) {{ {}1{} => 2 }}; }}",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    assert!(!errors(&src).is_empty());
    let src = format!("type T = {}i32;", "() => ".repeat(5000));
    assert!(!errors(&src).is_empty());
    let src = format!("type T = {}i32{};", "Array<".repeat(5000), ">".repeat(5000));
    assert!(!errors(&src).is_empty());
    // Moderate nesting is fine.
    let src = format!(
        "function f() {{ x = {}1{}; }}",
        "(".repeat(40),
        ")".repeat(40)
    );
    parse_ok(&src);
}

#[test]
fn nested_ternaries_are_not_exponential() {
    let mut s = String::from("x");
    for i in 0..60 {
        s = format!("c{} ? {} : y{}", i, s, i);
    }
    let start = std::time::Instant::now();
    let e = expr(&s);
    assert!(matches!(e.kind, ExprKind::Cond { .. }));
    assert!(start.elapsed().as_secs() < 5);
}

/// Linearity itself is checked by counting work, in `src/linear_tests.rs`; this checks the input
/// it uses parses and that pathological JSX and generic arrow attempts still finish.
#[test]
fn jsx_decided_by_the_parser_finishes() {
    let (m, d) = parse(&common::JSX_UNIT.repeat(100));
    assert!(d.is_empty(), "{:?}", &d[..d.len().min(3)]);
    assert_eq!(m.items.len(), 300);
    // Generic arrow attempts that fail late, and unclosed elements, still finish.
    for src in [
        format!("x = {};", "<a>(".repeat(20_000)),
        format!("x = {};", "{<a>(x".repeat(5_000)),
        format!("x = {};", "<T>(x: T".repeat(5_000)),
    ] {
        let start = std::time::Instant::now();
        let _ = parse(&src);
        assert!(start.elapsed().as_secs() < 5, "took {:?}", start.elapsed());
    }
}

#[test]
fn perf_100k_lines() {
    let unit = "function f(a: i64, b: i64): i64 {\n  let x = a * 2 + b / 3 - (a % 7);\n  if (x > 10 && b < 3) { return x; } else { x += 1; }\n  for (let i = 0; i < 10; i++) { console.log(`i=${i} x=${x}`, \"s\\n\"); }\n  return g<i64>(x, [1, 2, 3], { a: 1, b }) as i64;\n}\n";
    let lines = unit.lines().count();
    let src = unit.repeat(100_000 / lines + 1);
    let start = std::time::Instant::now();
    let (m, d) = parse(&src);
    let elapsed = start.elapsed();
    assert!(d.is_empty());
    assert!(m.items.len() > 10_000);
    eprintln!("parsed {} lines in {:?}", src.lines().count(), elapsed);
    // Generous bound so debug builds on slow CI machines pass; release is far faster.
    assert!(elapsed.as_secs_f64() < 5.0, "too slow: {:?}", elapsed);
}

/// Fuzz regression (difftest parse-exponential-parens): each nested `(` in a type used to double
/// the parse time (a failed function-type attempt re-parsed the inside), so 26 unclosed levels
/// took 15 s and an editor would hang while typing them.
#[test]
fn nested_parens_are_linear() {
    let start = std::time::Instant::now();
    let fuzzed = format!(
        "declare async function f(path: {}string): Promise<bool>;",
        "(".repeat(40)
    );
    assert!(!errors(&fuzzed).is_empty());
    let n = 100;
    let (open, close) = ("(".repeat(n), ")".repeat(n));
    parse_ok(&format!("const x: {open}i64{close} = 1;"));
    parse_ok(&format!(
        "declare function g(p: {open}string{close}): {open}i64{close};"
    ));
    parse_ok(&format!("type F = {open}(a: i64) => i64{close};"));
    parse_ok(&format!("function h() {{ x = {open}a{close}; }}"));
    parse_ok(&format!(
        "function h() {{ x = (a: {open}i64{close}) => a; }}"
    ));
    parse_ok(&format!(
        "function h() {{ x = {}a{}; }}",
        "(a) => (".repeat(n),
        ")".repeat(n)
    ));
    let commas = format!(
        "function h() {{ x = {}a{}; }}",
        "(a, ".repeat(n),
        ")".repeat(n)
    );
    assert!(!errors(&commas).is_empty());
    let elapsed = start.elapsed();
    assert!(elapsed.as_secs_f64() < 2.0, "too slow: {elapsed:?}");
}
