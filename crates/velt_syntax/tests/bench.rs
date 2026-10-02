//! Parse-time benchmark: lexing and parsing only, on code without JSX (a synthetic 100k-line
//! file, std and the examples) and on JSX mixed with generics (a synthetic 50k-line file, the JSX
//! goldens). Prints the best and the median of `VELT_BENCH_RUNS` runs (default 10) per input:
//!
//! `cargo test --release -p velt_syntax --test bench -- --ignored --nocapture`
//!
//! The inputs use only `parse_file`, so the file can be copied onto an older commit to compare;
//! `VELT_BENCH_ROOT=<checkout>` then reads std, the examples and the goldens from the same
//! checkout for both.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velt_common::FileId;
use velt_syntax::parse_file;

/// One function of ordinary code: arithmetic, control flow, templates, a generic call.
const PLAIN_UNIT: &str = "function f(a: i64, b: i64): i64 {\n  let x = a * 2 + b / 3 - (a % 7);\n  if (x > 10 && b < 3) { return x; } else { x += 1; }\n  for (let i = 0; i < 10; i++) { console.log(`i=${i} x=${x}`, \"s\\n\"); }\n  return g<i64>(x, [1, 2, 3], { a: 1, b }) as i64;\n}\n";

/// Components with nested elements, attributes, text, maps and fragments, next to generic
/// arrows, generic calls and comparisons. Written so that the parser before JSX moved into it
/// (#118) reads it the same way: `<T,>` rather than `<T>`, no `as<T>()`.
const JSX_UNIT: &str = r#"type RowProps = { item: Item; selected: bool };

function Row(props: RowProps): JSX.Element {
  const cls = props.selected ? "row selected" : "row";
  return (
    <tr class={cls} key={props.item.id}>
      <td>{props.item.id}</td>
      <td>
        <a href={`/items/${props.item.id}`}>it's {props.item.name}</a>
      </td>
      <td>{props.item.count < 10 ? <b>low</b> : <i>ok</i>}</td>
    </tr>
  );
}

const pick = <T,>(xs: T[], i: i64): T => xs[i];
const keep = <T extends Item>(xs: T[]): T[] => xs.filter((x) => x.count > 0);

function Table(props: { items: Item[] }): JSX.Element {
  const user = props.raw as User;
  const n = count<Item>(props.items);
  return (
    <>
      <h2>Items ({n})</h2>
      <table>
        {props.items.map((item, i) => <Row item={item} selected={i < 3} />)}
      </table>
      <p>
        Showing {n} of {props.total} &amp; more
      </p>
    </>
  );
}

"#;

/// Where the corpus files are read from: `VELT_BENCH_ROOT`, else this repository.
fn repo_root() -> PathBuf {
    match std::env::var_os("VELT_BENCH_ROOT") {
        Some(root) => PathBuf::from(root),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
    }
}

/// Every `.vlt` file under `dir`, recursively, in a stable order.
fn vlt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            vlt_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "vlt") {
            out.push(path);
        }
    }
}

fn read_all(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// `unit` repeated to about `lines` lines, as one file.
fn synthetic(unit: &str, lines: usize) -> Vec<String> {
    vec![unit.repeat(lines / unit.lines().count() + 1)]
}

fn std_and_examples() -> Vec<String> {
    let mut paths = Vec::new();
    vlt_files(&repo_root().join("std"), &mut paths);
    vlt_files(&repo_root().join("examples"), &mut paths);
    read_all(&paths)
}

fn jsx_goldens() -> Vec<String> {
    let mut paths = Vec::new();
    vlt_files(&repo_root().join("tests/golden"), &mut paths);
    vlt_files(&repo_root().join("std/jsx"), &mut paths);
    let mut files = read_all(&paths);
    files.retain(|src| src.contains("</") || src.contains("/>"));
    files
}

/// Parses every file of `input` once; the total time, not counting dropping the modules.
fn parse_all(input: &[String]) -> Duration {
    let start = Instant::now();
    let parsed: Vec<_> = input.iter().map(|src| parse_file(FileId(0), src)).collect();
    let elapsed = start.elapsed();
    drop(std::hint::black_box(parsed));
    elapsed
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn parse_timing() {
    let runs: usize = std::env::var("VELT_BENCH_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    // Small corpora are parsed several times per run so a run takes long enough to time.
    let inputs = [
        ("no JSX, 100k lines", synthetic(PLAIN_UNIT, 100_000), 1),
        ("std and examples", std_and_examples(), 10),
        (
            "JSX and generics, 50k lines",
            synthetic(JSX_UNIT, 50_000),
            1,
        ),
        ("JSX goldens", jsx_goldens(), 50),
    ];
    let mut samples = vec![Vec::new(); inputs.len()];
    // Interleaved, so a busy moment slows every input alike.
    for _ in 0..runs.max(1) {
        for ((_, input, repeat), times) in inputs.iter().zip(&mut samples) {
            let total: Duration = (0..*repeat).map(|_| parse_all(input)).sum();
            times.push(total / *repeat);
        }
    }
    for ((name, input, _), times) in inputs.iter().zip(&mut samples) {
        times.sort();
        let lines: usize = input.iter().map(|s| s.lines().count()).sum();
        eprintln!(
            "parse {name:<28} {:>3} files {lines:>7} lines: best {:>9.3} ms, median {:>9.3} ms",
            input.len(),
            times[0].as_secs_f64() * 1e3,
            times[times.len() / 2].as_secs_f64() * 1e3,
        );
    }
}
