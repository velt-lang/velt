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

/// Cutting a file anywhere must not panic the parser: each cut parses what is before it and what
/// is after it.
///
/// Every cut of every golden would be quadratic in the file sizes (each cut parses the whole
/// file), and it was the slowest test of the gate. So by default the full every-character sweep
/// covers the kitchen sink and the goldens the change touches (`VELT_ROBUSTNESS_FILES`, set by
/// `cargo xtask check` from the changed paths: comma-separated paths relative to the workspace
/// root, such as `tests/golden/lang/foo.vlt`), and every other golden is cut where each token
/// starts or ends, within its top-level item, plus a deterministic sample of the characters,
/// within the whole file ([`sampled_cuts`]). `VELT_ROBUSTNESS=full` sweeps every character of
/// every file; the nightly gate sets it, and `cargo xtask check` does for a change to the parser
/// (velt_syntax) or velt_common.
///
/// The work is cut into shards, one test each: a parallel test runner runs them as separate
/// processes (threads in one process mostly wait on each other in the kernel, allocating and
/// freeing the parser's large buffers).
fn prefixes_never_panic(shard: usize) {
    // One thread with the parser's stack for the whole shard instead of one per parse.
    velt_syntax::with_parser_stack(|| sweep(shard));
}

fn sweep(shard: usize) {
    let full = match std::env::var("VELT_ROBUSTNESS").as_deref() {
        Ok("full") => true,
        Ok("" | "sample") | Err(_) => false,
        Ok(other) => panic!("VELT_ROBUSTNESS={other}: expected `full` or `sample`"),
    };
    let touched = std::env::var("VELT_ROBUSTNESS_FILES").unwrap_or_default();
    let touched: Vec<&str> = touched.split(',').map(str::trim).collect();
    let root = workspace_root();
    let mut sources: Vec<(String, String)> = golden_files()
        .iter()
        .map(|f| {
            let rel = f.strip_prefix(&root).unwrap_or(f).to_string_lossy();
            (rel.replace('\\', "/"), std::fs::read_to_string(f).unwrap())
        })
        .collect();
    sources.push(("kitchen sink".into(), KITCHEN_SINK.to_string()));
    const CHUNK: usize = 64;
    let mut k = 0;
    for (path, src) in &sources {
        let every_char = full || path == "kitchen sink" || touched.contains(&path.as_str());
        let cuts = if every_char {
            src.char_indices().map(|(i, _)| (0, i, src.len())).collect()
        } else {
            sampled_cuts(path, src)
        };
        for chunk in cuts.chunks(CHUNK) {
            k += 1;
            if k % PREFIX_SHARDS != shard {
                continue;
            }
            for &(lo, i, hi) in chunk {
                let _ = parse(&src[lo..i]);
                let _ = parse(&src[i..hi]);
            }
        }
    }
}

/// Besides the token boundaries, one character in this many is cut, within its item.
const ITEM_STRIDE: usize = 7;
/// One character in this many is cut with the rest of the file after it.
const FILE_STRIDE: usize = 41;

/// The cuts `(lo, at, hi)` of a golden outside the change: `src[lo..at]` and `src[at..hi]` are
/// parsed. `lo` is the start of the top-level item around the cut (an item runs from its start
/// to the next item's, comments included): the parser is at the top level between items, so
/// what comes before the item doesn't matter to the cut, and the cost per cut is the item's
/// size instead of the file's.
///
/// - Where each token starts or ends (as the lexer sees the file), and every [`ITEM_STRIDE`]th
///   character (inside strings, comments and JSX text, which have few token boundaries): `hi` is
///   the end of the item.
/// - Every [`FILE_STRIDE`]th character: `hi` is the end of the file, which covers what a cut does
///   to the rest of it (a cut inside a string or template turns the code after it inside out).
///
/// The character samples start at an offset that depends on the file's path, so they are the
/// same on every run and every machine while different files cover different offsets.
fn sampled_cuts(path: &str, src: &str) -> Vec<(usize, usize, usize)> {
    let mut starts: Vec<usize> = parse(src)
        .0
        .items
        .iter()
        .map(|item| item.span.lo as usize)
        .filter(|&lo| lo < src.len())
        .collect();
    starts.push(0);
    starts.sort_unstable();
    starts.dedup();
    let item_around = |at: usize| {
        let k = starts.partition_point(|&s| s <= at) - 1;
        (starts[k], starts.get(k + 1).copied().unwrap_or(src.len()))
    };
    // FNV-1a: stable everywhere, unlike the standard library's hasher.
    let hash = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let sample = |stride: usize| {
        let offset = (hash % stride as u64) as usize;
        src.char_indices()
            .skip(offset)
            .step_by(stride)
            .map(|(at, _)| at)
    };
    let mut in_item = std::collections::BTreeSet::new();
    for r in velt_syntax::token_ranges(src) {
        in_item.extend([r.start as usize, r.end as usize]);
    }
    in_item.extend(sample(ITEM_STRIDE));
    let in_item = in_item
        .into_iter()
        .filter(|&at| at < src.len() && src.is_char_boundary(at))
        .map(|at| {
            let (lo, hi) = item_around(at);
            (lo, at, hi)
        });
    let to_end = sample(FILE_STRIDE).map(|at| (item_around(at).0, at, src.len()));
    in_item.chain(to_end).collect()
}

const PREFIX_SHARDS: usize = 4;

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
    prefixes_never_panic_3 = 3
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

/// 60 levels parse (exponential cost would never finish); `src/linear_tests.rs` counts the work.
#[test]
fn sixty_nested_ternaries_parse() {
    let mut s = String::from("x");
    for i in 0..60 {
        s = format!("c{} ? {} : y{}", i, s, i);
    }
    let e = expr(&s);
    assert!(matches!(e.kind, ExprKind::Cond { .. }));
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
        let _ = parse(&src);
    }
}

/// A 100k-line file parses. Its cost is counted by `src/linear_tests.rs` (linear growth) and the
/// `bench.rs` benchmark (speed), not here: a time limit is a guess on a loaded machine.
#[test]
fn parses_100k_lines() {
    let lines = PLAIN_UNIT.lines().count();
    let src = PLAIN_UNIT.repeat(100_000 / lines + 1);
    let (m, d) = parse(&src);
    assert!(d.is_empty());
    assert!(m.items.len() > 10_000);
}

/// Fuzz regression (difftest parse-exponential-parens): each nested `(` in a type used to double
/// the parse time (a failed function-type attempt re-parsed the inside), so 26 unclosed levels
/// took 15 s and an editor would hang while typing them. These parse as they should; the cost is
/// counted by `src/linear_tests.rs`.
#[test]
fn nested_parens_parse() {
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
}
