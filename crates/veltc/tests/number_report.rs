//! `numrep` from the command line: `velt build --report numbers` names the `number` variables
//! in loops that stay doubles, and debug builds store bounded `number` counters as integers
//! too (#539).

mod no_window;
mod test_dir;

/// `velt build main.vlt <args>` of `src`: (stdout, stderr).
fn build(src: &str, args: &[&str]) -> (String, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt"])
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    (String::from_utf8_lossy(&out.stdout).into_owned(), stderr)
}

const LOOPS: &str = "
function main() {
  const xs: number[] = [0.5, 1.5, 2.5];
  let total = 0.0;
  let count = 0.0;
  for (let k = 0.0; k < 3.0; k++) {
    total += xs[k];
    count = count + 1;
  }
  let h = 0.0;
  for (let i = 0; i < 10; i++) {
    h = h / 2 + i;
  }
  console.log(total, count, h);
}
";

#[test]
fn report_names_the_doubles_and_why() {
    for mode in [
        &["--report", "numbers"][..],
        &["--release", "--report", "numbers"],
    ] {
        let (_, stderr) = build(LOOPS, &[&["--emit", "vir"], mode].concat());
        assert!(
            stderr.contains("numbers: 2 `number` variables in loops stay doubles"),
            "{stderr}"
        );
        assert!(
            stderr.contains("`total`: it may hold a fraction"),
            "{stderr}"
        );
        assert!(stderr.contains("`h`: it may hold a fraction"), "{stderr}");
        // Nothing compares `count`, but it counts the iterations of a bounded loop.
        assert!(!stderr.contains("`count`"), "{stderr}");
        assert!(
            stderr.contains("main.vlt:7:5: `total`"),
            "locations: {stderr}"
        );
        // `k` is whole and bounded by the loop test: an integer.
        assert!(!stderr.contains("`k`"), "{stderr}");
    }
}

#[test]
fn debug_builds_keep_bounded_number_counters_in_integers() {
    let (vir, _) = build(LOOPS, &["--emit", "vir"]);
    let main = vir
        .split("\nfn#")
        .find(|f| f.contains("_V4main()"))
        .expect("main in the VIR");
    assert!(main.contains("// k\n") || main.contains("// k"), "{main}");
    for line in main.lines().filter(|l| l.trim_end().ends_with("// k")) {
        assert!(
            line.contains(": i32") || line.contains(": i64"),
            "the counter `k` is an integer in debug builds too: {line}"
        );
    }
    // Indexing with it needs no `__floatIndex` call.
    assert!(!main.contains("floatIndex"), "{main}");
}
