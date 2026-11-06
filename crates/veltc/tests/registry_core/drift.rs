//! The `package.velt` cases that keep registry-core's reader from drifting from vpm's: every Velt
//! keyword where a manifest can hold a word (read from the Velt lexer's own list, so a keyword
//! added there is tested without anyone writing a case), seeded random mutations of a few valid
//! manifests (several edits at once, splices, escapes in odd places), the `tsCompat` overlap
//! rules over every short list, and 64 KiB manifests of one field, which must stay fast.

use std::path::Path;

use crate::manifests::{FULL, HEAD};

/// The words between `("` and `",` in the Velt lexer's keyword table, and the quoted words of
/// registry-core's own copy: a word either side adds is tested.
fn keywords(root: &Path) -> Vec<String> {
    let token = std::fs::read_to_string(root.join("crates/velt_syntax/src/lexer/token.rs"))
        .expect("read token.rs");
    let ours =
        std::fs::read_to_string(root.join("packages/registry-core/src/manifest/symbol_names.velt"))
            .expect("read symbol_names.velt");
    let mut words = vec![];
    for piece in token.split("(\"").skip(1) {
        if let Some((word, rest)) = piece.split_once('"') {
            if rest.starts_with(", ") && word.bytes().all(|b| b.is_ascii_lowercase()) {
                words.push(word.to_string());
            }
        }
    }
    assert!(
        words.len() >= 40 && words.iter().any(|w| w == "yield"),
        "the keyword table in token.rs changed shape; update keywords() ({words:?})"
    );
    for line in ours.lines() {
        let t = line.trim();
        if let Some(word) = t.strip_prefix('"').and_then(|t| t.strip_suffix("\",")) {
            if word.bytes().all(|b| b.is_ascii_lowercase()) {
                words.push(word.to_string());
            }
        }
    }
    words.sort();
    words.dedup();
    words
}

/// Each keyword as a bracketed key, a plain key, a `Symbol.` member, and an import's name or
/// alias; plus every well-known symbol, as keys that reach the manifest's JSON.
pub fn keyword_cases(root: &Path) -> Vec<String> {
    let host = |key: &str| {
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", hostModules: {{ {key}: \"./src/k.mjs\" }} }};")
    };
    let manifest = "export const pkg: Package = { name: \"a\", version: \"1.0.0\" };";
    let mut out = vec![];
    for kw in keywords(root) {
        out.push(host(&format!("[{kw}]")));
        out.push(host(&kw));
        out.push(host(&format!("[Symbol.{kw}]")));
        out.push(format!(
            "import type {{ {kw} }} from \"velt/package\";\n{manifest}"
        ));
        out.push(format!(
            "import type {{ Package as {kw} }} from \"velt/package\";\n{manifest}"
        ));
        out.push(format!(
            "import type {{ {kw} as Package }} from \"velt/package\";\n{manifest}"
        ));
        out.push(format!(
            "export const {kw}: Package = {{ name: \"a\", version: \"1.0.0\" }};"
        ));
    }
    for name in velt_syntax::JS_WELL_KNOWN_SYMBOLS {
        out.push(host(&format!("[Symbol.{name}]")));
    }
    out
}

/// Folder names whose overlaps, case and Unicode lowercasing (final sigma, dotted I) the
/// `tsCompat` checks decide.
const FOLDERS: &[&str] = &[
    "a", "A", "a/b", "A/b", "a/B", "a/b/c", "b", "ab", "a/bc", "b/a", "aΣ/b", "aς/b", "İ", "i̇",
];

/// `tsCompat` lists: every pair and triple of [`FOLDERS`], then a probe after two accepted folders
/// that it clashes with in the same way, to check which one the message names.
pub fn ts_compat_cases() -> Vec<String> {
    let case = |dirs: &[&str]| {
        let list: Vec<String> = dirs.iter().map(|d| format!("\"{d}\"")).collect();
        format!(
            "{HEAD}{{ name: \"a\", version: \"1.0.0\", tsCompat: [{}] }};",
            list.join(", ")
        )
    };
    let mut out = vec![];
    for a in FOLDERS {
        for b in FOLDERS {
            out.push(case(&[a, b]));
            for c in FOLDERS {
                out.push(case(&[a, b, c]));
            }
        }
    }
    for dirs in [
        ["a/b/c", "a/d", "a"],
        ["a/d", "a/b/c", "a"],
        ["x", "a/b/c", "A/B"],
        ["a/b/c", "a/b/d", "a/b"],
        ["a", "b", "a/b/c"],
        ["b", "a", "a/b/c"],
        ["a/b", "a/c", "A/b"],
    ] {
        out.push(case(&dirs));
    }
    out
}

/// A manifest of one field holding `entry(i)` for i = 0, 1, … until about 63 KiB.
fn big(field: &str, open: &str, close: &str, entry: impl Fn(usize) -> String) -> String {
    let head = format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", {field}: {open}");
    let tail = format!("{close} }};");
    let mut body = String::new();
    let mut i = 0;
    while head.len() + body.len() + tail.len() < 63 * 1024 {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&entry(i));
        i += 1;
    }
    format!("{head}{body}{tail}")
}

/// 64 KiB manifests of one long field: checks over all of its entries must not be quadratic.
pub fn big_cases() -> Vec<String> {
    vec![
        big("tsCompat", "[", "]", |i| format!("\"d{i}\"")),
        big("tsCompat", "[", "]", |i| format!("\"p/q/d{}\"", i % 4000)),
        big("keywords", "[", "]", |i| format!("\"k{i}\"")),
        big("keywords", "[", "]", |i| format!("\"k{}\"", i % 3000)),
        big("define", "{", "}", |i| format!("D{i}:\"{i}.5e3\"")),
        big("define", "{", "}", |i| format!("D{i}:\"{i}.5x\"")),
        big("hostModules", "{", "}", |i| format!("m{i}:\"./src/m.mjs\"")),
        big("dependencies", "{", "}", |i| format!("d{i}:\"^1\"")),
    ]
}

/// What a random mutation inserts.
const INSERTS: &[&str] = &[
    ",",
    ";",
    ":",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "{}",
    "[]",
    ".",
    "...",
    "?",
    "!",
    "-",
    "+",
    "=",
    "/*c*/",
    "//c\n",
    "\n",
    "\r\n",
    "\u{2028}",
    "\u{2029}",
    "\u{feff}",
    "\t",
    "\u{a0}",
    "0",
    "1e400",
    "0x1F",
    "0o7",
    "0b1",
    "017",
    "08",
    "1_0",
    "1.",
    ".5",
    "5e-324",
    "1i32",
    "\"x\"",
    "'y'",
    "\"\\u{1F600}\"",
    "\"\\uD83D\\uDE00\"",
    "\"\\uD800\"",
    "\"\\u{110000}\"",
    "\"\\101\"",
    "\"\\8\"",
    "\"\\x4\"",
    "\"\\\r\n\"",
    "`t`",
    "/re/",
    "as const",
    "true",
    "null",
    "undefined",
    "__proto__",
    "[Symbol.iterator]",
    "[FOO]",
    "x()",
    "new X",
    "Σ",
    "İ",
    "é",
    "\u{301}",
    "\\",
];

/// The xorshift64* generator: deterministic, so a failing case is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// `count` manifests, each one to four random edits of a seed manifest: a character replaced, a
/// range deleted or doubled, the text cut short, a range spliced in from another seed, or an
/// [`INSERTS`] entry put in.
pub fn fuzz_cases(count: usize) -> Vec<String> {
    let seeds: Vec<Vec<char>> = [
        format!("{HEAD}{FULL};\n"),
        format!("{HEAD}{{ name: \"a\", version: \"1.0.0\", keywords: [\"k\"], native: {{ wasm: false }} }};\n"),
        "export const pkg: Package = { name: \"a\", version: \"1.0.0\", host: { target: \"a-b\", condition: \"edge\", entry: \"./src/e.mjs\", scaffold: { 7: \"./src/x\", [y]: \"./src/y\" } } }".into(),
        format!("{HEAD}{{ name: \"@s/a\", version: \"0.0.1\", tsCompat: [\"src/a\", \"lib\"], define: {{ A: \"1\" }} }};"),
    ]
    .iter()
    .map(|s| s.chars().collect())
    .collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut out = vec![];
    for _ in 0..count {
        let mut text = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(4) {
            let n = text.len();
            let at = rng.below(n + 1);
            let end = (at + 1 + rng.below(12)).min(n);
            match rng.below(6) {
                0 if at < n => text[at] = INSERTS[rng.below(INSERTS.len())].chars().next().unwrap(),
                1 => {
                    text.drain(at..end);
                }
                2 => {
                    let piece: Vec<char> = text[at..end].to_vec();
                    text.splice(at..at, piece);
                }
                3 => text.truncate(at),
                4 => {
                    let other = &seeds[rng.below(seeds.len())];
                    let from = rng.below(other.len());
                    let to = (from + 1 + rng.below(40)).min(other.len());
                    text.splice(at..at, other[from..to].iter().copied());
                }
                _ => {
                    let insert = INSERTS[rng.below(INSERTS.len())];
                    text.splice(at..at, insert.chars());
                }
            }
        }
        out.push(text.into_iter().collect());
    }
    out
}
