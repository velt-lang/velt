//! The timing lint: a test that waits a fixed time (`sleep(`, `delay(`, `setTimeout(`,
//! `setInterval(`) and then assumes something happened meanwhile passes on an idle machine and
//! fails on a loaded one, often only on Windows or macOS, and then in the merge queue of an
//! unrelated pull request. Tests order events by awaits, channels or observed state
//! (CONTRIBUTING.md "Before opening a pull request", item 6).
//!
//! Where time is what a test checks (an idle timeout, a timer's order, a guard timer that must
//! never fire), a marker comment says so: `// timing: <reason>`, on the line, in the comment
//! block right above it, at the top of the test (`it(`, `test(`, `describe(`, `function test…`,
//! or the comments above it), or at the top or the end of the file (before its first line of
//! code or after its last), which covers the whole file.
//!
//! Checked: `velt test` files (`*.test.velt`, `.test.ts`, `.test.tsx`), every `.velt` file under
//! `tests/std/`, and the goldens outside `tests/golden/lang/` and `tests/golden/m1/`…`m4/`. Those
//! are language programs on one event loop (they run on WebAssembly too, without threads,
//! sockets or the file system): timers there fire in deadline order, as in Node, so a wait
//! orders nothing outside the program; the nightly `flake` job repeats the async ones.
//!
//! `cargo xtask check` (lint part) checks every such file; `cargo xtask precheck` the changed ones.

/// The calls that wait.
const WAITS: &[&str] = &["sleep", "delay", "setTimeout", "setInterval"];

/// The marker, followed by a reason.
const MARKER: &str = "timing:";

/// Whether the lint checks `path` (relative, `/`-separated).
pub fn linted(path: &str) -> bool {
    let test_file = [".test.velt", ".test.ts", ".test.tsx"]
        .iter()
        .any(|s| path.ends_with(s));
    if test_file && !path.starts_with("tests/golden/") {
        return true;
    }
    if !path.ends_with(".velt") {
        return false;
    }
    if path.starts_with("tests/std/") {
        return true;
    }
    let Some(rest) = path.strip_prefix("tests/golden/") else {
        return false;
    };
    let category = rest.split('/').next().unwrap_or("");
    let language = category == "lang"
        || (category.len() == 2
            && category.starts_with('m')
            && category[1..].chars().all(|c| c.is_ascii_digit()));
    !language
}

/// The findings in `files` (relative to `root`; those the lint covers), with files that can't
/// be read reported too.
pub fn check_files(root: &std::path::Path, files: &[String]) -> Vec<String> {
    let mut out = vec![];
    for file in files.iter().filter(|f| linted(f)) {
        match std::fs::read_to_string(root.join(file)) {
            Ok(text) => out.extend(check(file, &text)),
            Err(e) => out.push(format!("{file}: cannot read it: {e}")),
        }
    }
    out
}

/// The findings in `text` (the file `path`): `path:line: …`, one per unmarked wait.
pub fn check(path: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let header_marked = lines
        .iter()
        .map(|l| l.trim())
        .take_while(|l| {
            l.is_empty() || l.starts_with("//") || l.starts_with("/*") || l.starts_with('*')
        })
        .any(has_marker);
    // At the end, where it shifts no line a golden's expected output names.
    let footer_marked = lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.is_empty() || l.starts_with("//"))
        .any(has_marker);
    if header_marked || footer_marked {
        return vec![];
    }
    let mut out = vec![];
    for (i, line) in lines.iter().enumerate() {
        let code = code_part(line);
        let Some(wait) = waits_in(code) else {
            continue;
        };
        if has_marker(line) || marked_above(&lines, i) || marked_in_test(&lines, i) {
            continue;
        }
        out.push(format!(
            "{path}:{}: `{wait}(` waits a fixed time in a test: order events by awaits, channels \
             or observed state, not by time; where time is what the test checks, say so with \
             `// timing: <reason>` (on the line, above it, at the top of the test, or at the top or \
             end of the file)",
            i + 1
        ));
    }
    out
}

/// A `// timing: <reason>` comment with a reason of at least three characters.
fn has_marker(line: &str) -> bool {
    let Some(at) = line.find("//") else {
        return false;
    };
    let comment = line[at + 2..].trim_start();
    comment
        .strip_prefix(MARKER)
        .is_some_and(|reason| reason.trim().chars().count() >= 3)
}

/// The comment block right above line `i` has a marker.
fn marked_above(lines: &[&str], i: usize) -> bool {
    lines[..i]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with("//"))
        .any(has_marker)
}

/// A marker between the start of the test around line `i` (or the comments above that start)
/// and line `i`, for the nearest enclosing `it(`/`test(`/`function test…` and `describe(`.
fn marked_in_test(lines: &[&str], i: usize) -> bool {
    let starts = |pred: fn(&str) -> bool| lines[..=i].iter().rposition(|l| pred(l.trim_start()));
    let is_test = |l: &str| {
        ["it(", "test(", "it.", "test."]
            .iter()
            .any(|p| l.starts_with(p))
            || {
                let l = l.strip_prefix("export ").unwrap_or(l);
                let l = l.strip_prefix("async ").unwrap_or(l);
                l.starts_with("function test")
            }
    };
    let is_describe = |l: &str| l.starts_with("describe(") || l.starts_with("describe.");
    [starts(is_test), starts(is_describe)]
        .into_iter()
        .flatten()
        .any(|start| lines[start..=i].iter().any(|l| has_marker(l)) || marked_above(lines, start))
}

/// `line` without a trailing `//` comment and without the contents of string literals.
fn code_part(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                    out.push(c);
                }
            }
            None if c == '/' && chars.peek() == Some(&'/') => break,
            None => {
                if c == '"' || c == '\'' || c == '`' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// The first wait called in `code`, unless it is a function's definition.
fn waits_in(code: String) -> Option<&'static str> {
    for wait in WAITS {
        let call = format!("{wait}(");
        for (at, _) in code.match_indices(&call) {
            let before = &code[..at];
            let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
            if before.chars().next_back().is_some_and(ident) {
                continue;
            }
            let before = before.trim_end();
            if before.ends_with("function") || before.ends_with("function*") {
                continue;
            }
            return Some(wait);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lint_covers_tests_and_goldens_outside_the_language_ones() {
        for path in [
            "packages/http-server/tests/h2/limits.test.velt",
            "std/text/format.test.velt",
            "examples/apps/chat/tests/room.test.ts",
            "tests/std/fetch/_server.velt",
            "tests/golden/std/timers.velt",
            "tests/golden/async_dispatch/pending.velt",
        ] {
            assert!(linted(path), "{path}");
        }
        for path in [
            "packages/http-server/src/server.velt",
            "std/prelude/timers.velt",
            "tests/golden/lang/timer_order.velt",
            "tests/golden/m3/async_basic.velt",
            "tests/golden/std/timers.out",
            "tests/std/README.md",
        ] {
            assert!(!linted(path), "{path}");
        }
    }

    #[test]
    fn an_unmarked_wait_is_reported_with_its_line() {
        let text = "import { sleep } from \"velt/time\";\n\nit(\"x\", async () => {\n  \
                    startServer();\n  await sleep(50);\n  expect(seen).toBe(1);\n});\n";
        let found = check("t.test.velt", text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("t.test.velt:5: `sleep(`"), "{found:?}");
        for wait in [
            "delay(5)",
            "setTimeout(() => done(), 10)",
            "setInterval(tick, 1)",
        ] {
            let found = check("t.test.velt", &format!("let x = 1;\n{wait};\n"));
            assert_eq!(found.len(), 1, "{wait}: {found:?}");
        }
        // A method call waits too.
        assert_eq!(
            check("t.test.velt", "x;\nawait timers.sleep(1);\n").len(),
            1
        );
    }

    #[test]
    fn definitions_comments_strings_and_other_names_are_not_waits() {
        let text = "x;\nasync function sleep(ms: number) {}\nfunction delay(ms: number) {}\n\
                    // await sleep(5) would race\nconst s = \"sleep(5)\";\nnosleep(1);\n\
                    clearTimeout(t);\nawait asleep(3);\n";
        assert_eq!(check("t.test.velt", text), Vec::<String>::new());
    }

    #[test]
    fn a_marker_covers_its_line_the_lines_below_its_test_or_its_file() {
        let on_line = "x;\nawait sleep(5); // timing: idle timeout is the subject\n";
        assert!(check("t.test.velt", on_line).is_empty());
        let above = "x;\n// The server closes idle connections.\n// timing: idle timeout\n\
                     await sleep(5);\n";
        assert!(check("t.test.velt", above).is_empty());
        let test = "x;\n// timing: the keep-alive timer is the subject\nit(\"idle\", async () => {\n  \
                    open();\n  await sleep(5);\n  await sleep(10);\n});\nit(\"other\", async () => {\n  \
                    await sleep(1);\n});\n";
        let found = check("t.test.velt", test);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("t.test.velt:9:"), "{found:?}");
        let in_test = "x;\nexport async function test_idle() {\n  // timing: idle timeout\n  \
                       open();\n  await sleep(5);\n}\n";
        assert!(check("t.test.velt", in_test).is_empty());
        let file = "// Timer tests.\n// timing: timers are the subject\n\nimport x;\n\
                    setTimeout(f, 1);\nawait sleep(2);\n";
        assert!(check("tests/golden/std/t.velt", file).is_empty());
        let footer = "import x;\nawait sleep(2);\n\n// timing: timers are the subject\n";
        assert!(check("tests/golden/std/t.velt", footer).is_empty());
        let describe = "x;\n// timing: timeouts are the subject\ndescribe(\"t\", () => {\n  \
                        it(\"a\", async () => {\n    await sleep(5);\n  });\n});\n";
        assert!(check("t.test.velt", describe).is_empty());
    }

    #[test]
    fn a_marker_needs_a_reason_and_a_gap_breaks_the_comment_block() {
        assert_eq!(
            check("t.test.velt", "x;\nawait sleep(5); // timing:\n").len(),
            1
        );
        assert_eq!(
            check(
                "t.test.velt",
                "x;\n// timing: idle timeout\n\nawait sleep(5);\n"
            )
            .len(),
            1
        );
        // A marker in a file's header needs to come before its code.
        assert_eq!(
            check(
                "t.test.velt",
                "x;\n// timing: late\nf();\nawait sleep(5);\n"
            )
            .len(),
            1
        );
    }
}
