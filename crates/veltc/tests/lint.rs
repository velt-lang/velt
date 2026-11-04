//! The lint rules of `velt check` through the `velt` binary (docs/internals/design/lint-profiles.md):
//! `--perf` hints, the levels `lint` in package.vlt sets, `velt-allow` comments, `--fix` and
//! `--fix=review` (which undoes a fix after which the program no longer checks), the profiles
//! (a `tsCompat` folder gets only fixes that stay TypeScript), the evidence rule on the optimized
//! program, and the JSON form.

use std::path::Path;
use std::process::Output;

use serde_json::Value;

mod no_window;
mod test_dir;

fn velt(cwd: &Path, args: &[&str]) -> Output {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

/// A package named `app` whose manifest object also has `fields`.
fn package(dir: &Path, fields: &str) {
    write(
        dir,
        "package.vlt",
        &format!(
            "import type {{ Package }} from \"velt/package\";\n\n\
             export const pkg: Package = {{ name: \"app\", version: \"0.1.0\"{fields} }};\n"
        ),
    );
}

/// The `code`s of the diagnostics in a `--json` report (`-` for the check's own).
fn codes(report: &Value) -> Vec<String> {
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap_or("-").to_string())
        .collect()
}

const SUM: &str = "export function sum(xs: number[]): number {
  let total = 0;
  xs.forEach((x) => { total += x; });
  return total;
}

function main() {
  console.log(sum([1, 2, 3]));
}
";

#[test]
fn perf_hints_show_with_perf_only() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir, "");
    write(dir, "src/main.vlt", SUM);
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!stderr(&o).contains("perf("), "{}", stderr(&o));
    let o = velt(dir, &["check", "--perf"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(
        err.contains("main.vlt:3:3: note: this `forEach` callback assigns `total`"),
        "{err}"
    );
    assert!(
        err.contains("= note: fix: rewrite as `for (const x of xs)` (`velt check --fix=review`)"),
        "{err}"
    );
    assert!(err.contains("= note: perf(foreach-capture)"), "{err}");
}

#[test]
fn levels_from_the_manifest() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(
        dir,
        ", lint: { perf: \"deny\", rules: { \"perf/nope\": \"off\" } }",
    );
    write(dir, "src/main.vlt", SUM);
    // `deny`: plain `velt check` runs the rule and fails.
    let o = velt(dir, &["check"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(err.contains("error: this `forEach` callback"), "{err}");
    assert!(
        err.contains("`lint.rules` names `perf/nope`, which is not a lint rule"),
        "{err}"
    );
    package(
        dir,
        ", lint: { perf: \"deny\", rules: { \"perf/foreach-capture\": \"off\" } }",
    );
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", stderr(&o));
    // A level the manifest can't have.
    package(dir, ", lint: { perf: \"loud\" }");
    let err = stderr(&velt(dir, &["check"]));
    assert!(
        err.contains("`lint.perf` must be \"off\", \"hint\", \"warn\" or \"deny\", not \"loud\""),
        "{err}"
    );
}

#[test]
fn allow_comments_need_a_reason_and_a_use() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir, ", lint: { perf: \"warn\" }");
    let allowed = SUM.replace(
        "  xs.forEach",
        "  // velt-allow(perf/foreach-capture): the order of calls matters here\n  xs.forEach",
    );
    write(dir, "src/main.vlt", &allowed);
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stderr(&o), "", "suppressed");
    let bare = SUM.replace(
        "  xs.forEach",
        "  // velt-allow(perf/foreach-capture)\n  xs.forEach",
    );
    write(dir, "src/main.vlt", &bare);
    let err = stderr(&velt(dir, &["check"]));
    assert!(
        err.contains("warning: this `velt-allow` comment gives no reason"),
        "{err}"
    );
    let unused = SUM.replace(
        "  return total;",
        "  // velt-allow(perf/class-as-data): x\n  return total;",
    );
    write(dir, "src/main.vlt", &unused);
    let err = stderr(&velt(dir, &["check"]));
    assert!(
        err.contains("`velt-allow(perf/class-as-data)` suppresses nothing"),
        "{err}"
    );
}

const SHAPES: &str = "class Point {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
}

export function sum(xs: number[]): number {
  let total = 0;
  xs.forEach((x) => { total += x; });
  return total;
}

function main() {
  const ps: Point[] = [];
  for (let i = 0; i < 3; i++) ps.push(new Point(i, 1));
  console.log(sum([ps[0].x, ps[2].y]));
}
";

/// Both fixes may change what the program does (the class's copies instead of sharing, the loop
/// also visits elements its body adds), so `--fix` applies neither and `--fix=review` both.
#[test]
fn review_fixes_need_fix_review() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir, "");
    write(dir, "src/main.vlt", SHAPES);
    let o = velt(dir, &["check", "--fix"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with("velt: nothing to fix\n"),
        "{}",
        stderr(&o)
    );
    assert_eq!(read(dir, "src/main.vlt"), SHAPES);
    let o = velt(dir, &["check", "--fix=review"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with("velt: fixed 2 problems in 1 file\n"),
        "{}",
        stderr(&o)
    );
    let after = read(dir, "src/main.vlt");
    assert!(
        after.starts_with("type Point = { x: number; y: number };\n"),
        "{after}"
    );
    assert!(
        after.contains("ps.push({ x: i, y: 1 } as Point)"),
        "{after}"
    );
    assert!(
        after.contains("for (const x of xs) { total += x; }"),
        "{after}"
    );
    assert!(velt(dir, &["check"]).status.success());
    let o = velt(dir, &["check", "--fix=review"]);
    assert_eq!(stderr(&o), "velt: nothing to fix\n");
}

/// `forEach` visits the elements there when it starts, `for…of` also those added on the way, and
/// a body can add one through a call, an alias, its owner's method, or with no call in its text
/// at all (a getter of the element), so the rewrite is always a Review fix, which `--fix` leaves
/// alone. Each program prints 6 (the getter one 3) with `forEach`, 26 (5) with `for…of`.
#[test]
fn a_foreach_rewrite_is_a_review_fix() {
    let programs = [
        "function main() {\n  const xs = [1, 2, 3];\n  let sum = 0;\n  xs.forEach((x) => { sum += x; });\n  console.log(sum);\n}\n",
        "class Item {\n  get v(): number { if (xs.length < 5) xs.push(new Item()); return 1; }\n}\nconst xs: Item[] = [new Item(), new Item(), new Item()];\nfunction main() {\n  let sum = 0;\n  xs.forEach((x) => { sum += x.v; });\n  console.log(sum);\n}\n",
        "function main() {\n  const xs = [1, 2, 3];\n  let sum = 0;\n  const grow = () => { if (xs.length < 5) xs.push(10); };\n  xs.forEach((x) => { sum += x; grow(); });\n  console.log(sum);\n}\n",
        "function main() {\n  const xs = [1, 2, 3];\n  const ys = xs;\n  let sum = 0;\n  xs.forEach((x) => { sum += x; if (ys.length < 5) ys.push(10); });\n  console.log(sum);\n}\n",
        "class Bag {\n  items: number[] = [1, 2, 3];\n  add(): void { if (this.items.length < 5) this.items.push(10); }\n}\nfunction main() {\n  const b = new Bag();\n  const items = b.items;\n  let sum = 0;\n  items.forEach((x) => { sum += x; b.add(); });\n  console.log(sum);\n}\n",
    ];
    for src in programs {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        write(dir, "main.vlt", src);
        let o = velt(dir, &["check", "main.vlt", "--perf", "--json"]);
        let report: Value = serde_json::from_slice(&o.stdout).unwrap();
        let d = report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["code"] == "perf/foreach-capture")
            .unwrap_or_else(|| panic!("no finding: {report:#}"));
        assert_eq!(d["fixes"][0]["applicability"], "review", "{src}");
        let o = velt(dir, &["check", "main.vlt", "--fix"]);
        assert_eq!(
            stderr(&o).lines().next(),
            Some("velt: nothing to fix"),
            "{src}"
        );
        assert_eq!(read(dir, "main.vlt"), src);
    }
}

/// A fix after which the program no longer checks is undone: typing the record parameter breaks
/// the caller that passes a dynamic record.
#[test]
fn a_fix_that_breaks_the_program_is_undone() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir, "");
    let src = "export function area(p: Record<string, any>): number {
  return (p[\"w\"] as number) * (p[\"h\"] as number);
}

function main() {
  const r: Record<string, any> = {};
  r[\"w\"] = 2;
  r[\"h\"] = 3;
  console.log(area(r));
}
";
    write(dir, "src/main.vlt", src);
    let o = velt(dir, &["check", "--fix=review"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(
        err.starts_with("velt: fixed 0 problems in 0 files\n"),
        "{err}"
    );
    assert!(
        err.contains("velt: not applied, the program did not check after it: type `p` as `{ w: number; h: number }`"),
        "{err}"
    );
    assert_eq!(read(dir, "src/main.vlt"), src);
}

/// The loop counter stays a double; in the package's own file the fix declares it `i64`, in a
/// `tsCompat` folder there is no fix (it would not be TypeScript), so the finding is a note.
#[test]
fn evidence_and_profiles() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(
        dir,
        ", tsCompat: [\"src/shared\"], lint: { perf: \"warn\" }",
    );
    let counter = |name: &str| {
        format!(
            "export function {name}(): void {{\n  let k = 0;\n  for (let r = 0; r < 100000000; r++) {{ k = k + r; }}\n  console.log(k);\n}}\n"
        )
    };
    write(dir, "src/shared/count.ts", &counter("shared"));
    write(
        dir,
        "src/main.vlt",
        &format!(
            "import {{ shared }} from \"./shared/count\";\n\n{}\nfunction main() {{\n  own();\n  shared();\n}}\n",
            counter("own")
        ),
    );
    let o = velt(dir, &["check", "--perf", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: Value = serde_json::from_slice(&o.stdout).unwrap();
    let found: Vec<&Value> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "perf/number-double")
        .collect();
    let by_file = |f: &str| {
        found
            .iter()
            .find(|d| d["location"]["file"].as_str().unwrap().ends_with(f))
            .unwrap_or_else(|| panic!("no finding in {f}: {report:#}"))
    };
    let own = by_file("main.vlt");
    assert_eq!(own["severity"], "warning");
    assert!(
        own["message"]
            .as_str()
            .unwrap()
            .starts_with("`k` stays a double"),
        "{own}"
    );
    assert_eq!(own["fixes"][0]["title"], "declare `k` as `i64`");
    assert_eq!(own["fixes"][0]["applicability"], "review");
    assert_eq!(own["fix"]["replacement"], ": i64");
    let shared = by_file("count.ts");
    assert_eq!(shared["severity"], "note");
    assert_eq!(shared["fixes"], Value::Array(vec![]));
    // Applied, the counter is an integer and the finding is gone.
    let o = velt(dir, &["check", "--perf", "--fix=review"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(read(dir, "src/main.vlt").contains("let k: i64 = 0;"));
    assert!(read(dir, "src/shared/count.ts").contains("let k = 0;"));
}

#[test]
fn json_has_codes_and_fixes() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir, "");
    write(dir, "src/main.vlt", SUM);
    let o = velt(dir, &["check", "--perf", "--json"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let report: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(codes(&report), ["perf/foreach-capture"]);
    let d = &report["diagnostics"][0];
    assert_eq!(d["severity"], "note");
    assert_eq!(d["fixes"][0]["applicability"], "review");
    assert_eq!(
        d["fixes"][0]["edits"][0]["replacement"],
        "for (const x of xs) { total += x; }"
    );
    assert_eq!(d["fix"]["title"], "rewrite as `for (const x of xs)`");
    assert_eq!(
        (report["errors"].as_u64(), report["warnings"].as_u64()),
        (Some(0), Some(0))
    );
}

/// `--fix` applies the ts-compat findings' fixes too.
#[test]
fn ts_compat_findings_are_fixed() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    write(
        dir,
        "a.vlt",
        "export function f(x: f64): bool {\n  return x > 1;\n}\n",
    );
    let o = velt(dir, &["check", "--ts-compat", "a.vlt", "--fix"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        read(dir, "a.vlt"),
        "export function f(x: number): boolean {\n  return x > 1;\n}\n"
    );
}
