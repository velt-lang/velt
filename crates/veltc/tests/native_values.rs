//! Arrays and structs across the native boundary, end to end (docs/internals/contracts/native_abi.md):
//! the test package `native_values/vals` (a path dependency, its library built from source with
//! cargo) passes `Row[]` and `f64[]` in, returns `Row[]`, a struct and, from an async export,
//! `IoResult<Row[]>`, and takes `Row[][]` (nested views, each freed after the call). The program shares `Row` and `string[]` values, so both are counted
//! (boxed) types and every value crosses through the compiler's foreign-layout conversion; the
//! async results are awaited directly, kept and awaited later, and combined with `Promise.all`.
//! It runs on both schedulers with the debug runtime's allocator checks and leak count.
//!
//! Needs cargo (the first run builds the SDK, a few seconds later); `VELT_SKIP_NATIVE_E2E=1`
//! skips it.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};
use std::process::Output;

fn package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_values/vals")
}

const PROGRAM: &str = r#"import { Row, count, later, laterRaw, reverse, stats } from "vals";

function show(rows: Row[]) {
  for (const r of rows) {
    console.log(r.id, r.name, r.tags.join("+"));
  }
}

async function main() {
  const rows: Row[] = [
    { id: 1, name: "a", tags: ["x", "y"] },
    { id: 2, name: "b", tags: [] },
  ];
  // Values referenced twice: `Row` and `string[]` become counted (boxed) types.
  const keep: Row[] = [];
  keep.push(rows[0]);
  const tags = rows[0].tags;
  const tags2 = tags;
  const first = rows[0];
  const again = first;
  rows[0].id = 1;
  show(reverse(rows));
  const s = stats([1.5, 2.5, 3]);
  console.log(s.count, s.sum);
  show(await later(3, ["p", "q"]));
  const kept = laterRaw(2, ["k"]);
  const both = await Promise.all([laterRaw(2, ["m"]), laterRaw(1, ["n"])]);
  show((await kept).value);
  show(both[0].value);
  show(both[1].value);
  const failed = await laterRaw(1, []);
  console.log(failed.code != 0, failed.message);
  console.log(keep.length, tags2.length, keep[0].name, first.id, again.name);
  // Arrays of arrays of a counted type: each inner array crosses as a view of its own.
  console.log(count([rows, rows, [rows[1]]]), count([]));
}
"#;

const EXPECTED: &str = "2 b \n1 a x+y\n3 7\n0 p \n1 q t0\n2 p t0+t1\n0 k \n1 k t0\n0 m \n1 m t0\n0 n \ntrue no names\n1 2 a 1 a\n5 0\n";

fn run(app: &Path, scheduler: &str) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.vlt"])
        .current_dir(app)
        .env("VELT_SCHEDULER", scheduler)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env("VELT_DEBUG_COUNTED", "1")
        .output()
        .expect("run velt")
}

#[test]
fn arrays_and_structs_cross_with_counted_types_on_both_schedulers() {
    if std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some_and(|v| v == "1") {
        eprintln!("skipped: VELT_SKIP_NATIVE_E2E=1");
        return;
    }
    let tmp = test_dir::TestDir::new();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let dep = package().to_string_lossy().replace('\\', "/");
    std::fs::write(
        app.join("package.vlt"),
        format!(
            "import type {{ Package }} from \"velt:package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  dependencies: {{ vals: {{ path: \"{dep}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::write(app.join("src/main.vlt"), PROGRAM).unwrap();
    for scheduler in ["stealing", "velt"] {
        let out = run(&app, scheduler);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            out.status.success(),
            "{scheduler}: velt run failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        );
        assert_eq!(stdout, EXPECTED, "{scheduler}; stderr:\n{stderr}");
        assert!(
            stderr.contains("counted types: Row, string[]"),
            "the program no longer counts `Row` and `string[]`:\n{stderr}"
        );
        let stats = stderr
            .lines()
            .find(|l| l.starts_with("rc stats:"))
            .unwrap_or_else(|| panic!("no rc stats in:\n{stderr}"));
        let blocks = stats
            .split_whitespace()
            .find_map(|w| w.strip_prefix("blocks="))
            .unwrap();
        let (alloc, free) = blocks.split_once('/').unwrap();
        assert_eq!(alloc, free, "{scheduler}: leaked blocks ({stats})");
    }
}
