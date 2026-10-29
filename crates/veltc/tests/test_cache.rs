//! `velt test` keeps the results of test files whose program didn't change and reaches nothing
//! outside itself (#1855 step 3): a pure file's report comes back as `(cached)`, a file that
//! reads the clock runs every time and says so, a change runs the file again, and a failure is
//! never kept.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

struct Package {
    _tmp: test_dir::TestDir,
    dir: PathBuf,
}

fn package() -> Package {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path().to_path_buf();
    let p = Package { _tmp: tmp, dir };
    p.write(
        "package.vlt",
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };\n",
    );
    p.write("src/lib.vlt", &lib("a + b"));
    p.write("tests/pure.test.vlt", &pure_test(5));
    p.write(
        "tests/clock.test.vlt",
        "import { expect } from \"velt:test\";\n\nexport function test_clock() {\n  expect(Date.now() > 0).toBe(true);\n}\n",
    );
    p
}

fn lib(body: &str) -> String {
    format!("export function add(a: number, b: number): number {{\n  return {body};\n}}\n")
}

fn pure_test(expected: i32) -> String {
    format!(
        "import {{ add }} from \"../src/lib\";\nimport {{ expect }} from \"velt:test\";\n\nexport function test_add() {{\n  expect(add(2, 3)).toBe({expected});\n}}\n"
    )
}

impl Package {
    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn test(&self, args: &[&str]) -> Output {
        self.test_with(args, &[])
    }

    fn test_with(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        cmd.arg("test")
            .args(args)
            .current_dir(&self.dir)
            .env("VELT_HOME", self.dir.join("home"))
            .env_remove("VELT_LAUNCHER")
            .env_remove("VELT_RC_STATS");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// The `running` lines of a `velt test` that must pass, paths relative to the package.
    fn running(&self, args: &[&str]) -> Vec<String> {
        let o = self.test(args);
        let out = String::from_utf8_lossy(&o.stdout).replace('\\', "/");
        assert!(
            o.status.success(),
            "{out}{}",
            String::from_utf8_lossy(&o.stderr)
        );
        // From `tests/` on: the printed path may be the canonical one (`/private/var/…`).
        out.lines()
            .filter_map(|l| l.strip_prefix("running "))
            .map(|l| l[l.find("tests/").unwrap_or(0)..].to_string())
            .collect()
    }
}

const CLOCK: &str = "tests/clock.test.vlt (ran: not cacheable, it uses the clock)";

#[test]
fn pure_test_files_are_not_run_again_until_their_program_changes() {
    let p = package();
    assert_eq!(p.running(&[]), [CLOCK, "tests/pure.test.vlt"]);
    assert_eq!(p.running(&[]), [CLOCK, "tests/pure.test.vlt (cached)"]);

    // A change in what the pure test's program imports runs it again, and keeps the new result.
    p.write("src/lib.vlt", &lib("b + a"));
    assert_eq!(p.running(&[]), [CLOCK, "tests/pure.test.vlt"]);
    assert_eq!(p.running(&[]), [CLOCK, "tests/pure.test.vlt (cached)"]);

    // `--no-cache` runs and says nothing of either.
    assert_eq!(
        p.running(&["--no-cache"]),
        ["tests/clock.test.vlt", "tests/pure.test.vlt"]
    );
}

#[test]
fn a_failure_is_never_kept() {
    let p = package();
    p.write("tests/pure.test.vlt", &pure_test(6));
    for _ in 0..2 {
        let o = p.test(&[]);
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(!o.status.success(), "{out}");
        assert!(
            out.contains("FAILED test_add") && !out.contains("(cached)"),
            "{out}"
        );
    }
}

/// A result kept without the leak check (`VELT_RC_STATS=1`) doesn't stand for a run with it.
#[test]
fn the_leak_check_runs_what_an_ordinary_run_kept() {
    let p = package();
    assert!(p.test(&[]).status.success());
    assert!(String::from_utf8_lossy(&p.test(&[]).stdout).contains("pure.test.vlt (cached)"));
    let o = p.test_with(&[], &[("VELT_RC_STATS", "1")]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("pure.test.vlt (cached)"), "{out}");
}
