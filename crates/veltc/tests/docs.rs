//! Doc tests: every ```` ```ts ```` block in the user-facing docs compiles (parse, sema, lowering,
//! VIR verification — no codegen or linking, so the whole set checks in seconds).
//!
//! Checked files: every Markdown file under [`DOCS`] (a file, or a directory searched
//! recursively). The fence's info string says what is expected:
//! - `ts` — must compile.
//! - `ts error` — must be rejected with a diagnostic (not an internal compiler error).
//! - `ts planned` — decided design that is not implemented yet: not compiled. The surrounding
//!   text must say so (the reference marks it **Planned**).
//! - `ts ignore` — not a complete program (an excerpt, a relative import, pseudo-code).
//!
//! A snippet without `main` is completed: declarations (`import`, `function`, `class`, `struct`,
//! `interface`, `type`, `enum`, `extend`, `export …`, `const UPPER_CASE`) stay at module level and
//! every other top-level statement moves, in order, into a generated `async function main()`.
//!
//! Filter with `VELT_DOCS=<substring of "file:line">`; `VELT_DOCS_DUMP=<dir>` also writes the
//! completed programs there. Owned by the docs agent.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use veltc::cli::Emit;
use veltc::driver::{self, BuildError, BuildOptions, Session};

/// The documents whose `ts` blocks are checked (paths relative to the repo root): Markdown
/// files, or directories whose `.md` files are checked recursively. `docs/internals` is not
/// listed: design notes show syntax that is not built yet.
const DOCS: &[&str] = &[
    "README.md",
    "docs/book",
    "docs/reference",
    "docs/std",
    "docs/tooling",
];

/// What a fenced block's info string asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Compiles,
    Fails,
    Skipped,
}

/// One fenced `ts` block.
struct Snippet {
    /// `docs/book/tour.md:42` (the fence line).
    origin: String,
    expect: Expect,
    source: String,
}

/// The Markdown files of `rel` (a file, or a directory searched recursively), relative to `root`
/// with `/` separators, sorted.
fn markdown_files(root: &Path, rel: &str) -> Vec<String> {
    let path = root.join(rel);
    if path.is_file() {
        return vec![rel.to_string()];
    }
    let mut out = vec![];
    let entries = std::fs::read_dir(&path).unwrap_or_else(|e| panic!("{rel}: {e}"));
    for entry in entries {
        let name = entry.expect("directory entry").file_name();
        let child = format!("{rel}/{}", name.to_string_lossy());
        if root.join(&child).is_dir() || child.ends_with(".md") {
            out.extend(markdown_files(root, &child));
        }
    }
    out.sort();
    out
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// Extracts the `ts` blocks of a Markdown file; an unknown `ts …` info string is an error.
fn snippets(rel: &str, text: &str, errors: &mut Vec<String>) -> Vec<Snippet> {
    let mut out = vec![];
    let mut lines = text.lines().enumerate();
    while let Some((i, line)) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let indent = line.len() - trimmed.len();
        let mut body = vec![];
        for (_, l) in lines.by_ref() {
            if l.trim_start().starts_with("```") {
                break;
            }
            body.push(l.get(indent..).unwrap_or(l.trim_start()));
        }
        let mut words = info.split_whitespace();
        if words.next() != Some("ts") {
            continue;
        }
        let origin = format!("{rel}:{}", i + 1);
        let expect = match words.next() {
            None => Expect::Compiles,
            Some("error") => Expect::Fails,
            Some("planned" | "ignore") => Expect::Skipped,
            Some(other) => {
                errors.push(format!(
                    "{origin}: unknown info string `ts {other}` (use ts, ts error, ts planned or ts ignore)"
                ));
                continue;
            }
        };
        out.push(Snippet {
            origin,
            expect,
            source: body.join("\n") + "\n",
        });
    }
    out
}

/// Module-level declarations stay where they are; other top-level statements go into `main`.
fn is_declaration(first_line: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "import ",
        "export ",
        "function ",
        "async function ",
        "class ",
        "struct ",
        "interface ",
        "type ",
        "enum ",
        "extend ",
        "extend<",
        "declare ",
    ];
    if KEYWORDS.iter().any(|k| first_line.starts_with(k)) {
        return true;
    }
    // `const MAX = 100;`: a module constant, by naming convention.
    first_line
        .strip_prefix("const ")
        .and_then(|rest| rest.split([' ', ':', '=']).next())
        .is_some_and(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
}

/// The program a snippet stands for (see the module docs).
fn complete(source: &str) -> String {
    let has_main = source.lines().any(|l| {
        l.starts_with("function main(")
            || l.starts_with("async function main(")
            || l.starts_with("export function main(")
    });
    if has_main {
        return source.to_string();
    }
    // Split into top-level items: an item starts at a non-blank line without indentation that
    // does not close a bracket; column-0 comments belong to the item that follows them.
    let mut items: Vec<Vec<&str>> = vec![];
    let mut pending_comments: Vec<&str> = vec![];
    for line in source.lines() {
        let starts_item = !line.is_empty()
            && !line.starts_with([' ', '\t', '}', ')', ']'])
            && !line.starts_with("//");
        if line.starts_with("//") {
            pending_comments.push(line);
        } else if starts_item || items.is_empty() {
            let mut item = std::mem::take(&mut pending_comments);
            item.push(line);
            items.push(item);
        } else {
            let last = items.last_mut().expect("an item");
            last.append(&mut pending_comments);
            last.push(line);
        }
    }
    let mut decls = String::new();
    let mut stmts = String::new();
    for item in &items {
        let first = item
            .iter()
            .find(|l| !l.starts_with("//") && !l.trim().is_empty())
            .copied()
            .unwrap_or("");
        let target = if is_declaration(first) {
            &mut decls
        } else {
            &mut stmts
        };
        for l in item {
            target.push_str(l);
            target.push('\n');
        }
    }
    if stmts.trim().is_empty() {
        format!("{decls}\nfunction main() {{}}\n")
    } else {
        let body: String = stmts.lines().map(|l| format!("  {l}\n")).collect();
        format!("{decls}\nasync function main() {{\n{body}}}\n")
    }
}

/// Compiles a program to verified VIR. `Ok(())`, or `Err((is_ice, message))`.
fn compile(root: &Path, program: &str) -> Result<(), (bool, String)> {
    let opts = BuildOptions {
        // Virtual root module path: relative imports resolve from the repo root.
        input: root.join("__doc_snippet.vlt"),
        root_source: Some(program.to_string()),
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    let result = catch_unwind(AssertUnwindSafe(|| driver::compile(&mut sess, &opts)));
    match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(BuildError::Diagnostics)) => Err((false, sess.render_diagnostics())),
        Ok(Err(BuildError::Failed(msg))) => Err((
            false,
            format!("{}\nerror: {msg}", sess.render_diagnostics()),
        )),
        Ok(Err(BuildError::Ice(msg))) => Err((true, format!("internal compiler error: {msg}"))),
        Err(_) => Err((true, "the compiler panicked".to_string())),
    }
}

fn check(root: &Path, s: &Snippet) -> Option<String> {
    let program = complete(&s.source);
    // `VELT_DOCS_DUMP=<dir>`: also write each completed program there (to run them by hand and
    // compare with the outputs the docs claim).
    if let Some(dir) = std::env::var_os("VELT_DOCS_DUMP") {
        let name = s.origin.replace(['/', '.', ':'], "_") + ".vlt";
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(Path::new(&dir).join(name), &program);
    }
    let result = compile(root, &program);
    let shown = || {
        program
            .lines()
            .enumerate()
            .map(|(i, l)| format!("{:>4} | {l}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match (s.expect, result) {
        (Expect::Compiles, Ok(())) | (Expect::Fails, Err((false, _))) => None,
        (Expect::Compiles, Err((_, msg))) => Some(format!(
            "{}: does not compile (fix it, or tag it `ts planned` / `ts ignore`)\n{msg}\n--- program ---\n{}",
            s.origin,
            shown()
        )),
        (Expect::Fails, Ok(())) => Some(format!(
            "{}: tagged `ts error` but compiles\n--- program ---\n{}",
            s.origin,
            shown()
        )),
        (Expect::Fails, Err((true, msg))) => Some(format!(
            "{}: tagged `ts error`: must fail with a diagnostic, not an ICE\n{msg}",
            s.origin
        )),
        (Expect::Skipped, _) => None,
    }
}

#[test]
fn doc_snippets_compile() {
    let root = root();
    let filter = std::env::var("VELT_DOCS").unwrap_or_default();
    let mut errors = vec![];
    let mut all = vec![];
    for rel in DOCS.iter().flat_map(|d| markdown_files(&root, d)) {
        let text = std::fs::read_to_string(root.join(&rel)).expect("doc file");
        all.extend(snippets(&rel, &text, &mut errors));
    }
    let (todo, skipped): (Vec<&Snippet>, Vec<&Snippet>) = all
        .iter()
        .filter(|s| s.origin.contains(&filter))
        .partition(|s| s.expect != Expect::Skipped);

    let next = AtomicUsize::new(0);
    let failures = Mutex::new(errors);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(s) = todo.get(i) else { break };
                if let Some(f) = check(&root, s) {
                    failures.lock().unwrap().push(f);
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    println!(
        "docs: {} snippets checked, {} not compiled (planned/ignore), {} failures",
        todo.len(),
        skipped.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
fn completes_snippets_without_main() {
    let src = "import { x } from \"velt:a\";\nconst MAX = 3;\n// note\nclass A {\n}\nconst a = new A();\nfor (const i of [1]) {\n  console.log(i);\n}\n";
    let want = "import { x } from \"velt:a\";\nconst MAX = 3;\n// note\nclass A {\n}\n\nasync function main() {\n  const a = new A();\n  for (const i of [1]) {\n    console.log(i);\n  }\n}\n";
    assert_eq!(complete(src), want);
    assert_eq!(
        complete("function f() {}\n"),
        "function f() {}\n\nfunction main() {}\n"
    );
    let with_main = "function main() {}\n";
    assert_eq!(complete(with_main), with_main);
}
