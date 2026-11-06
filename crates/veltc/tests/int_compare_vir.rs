//! `numrep` compares an unsigned counter with a bound that may be negative (`a.length - 15`) in
//! integers, as `n > 0 & i < n as u64` (`int_compare`), as its last step: what narrowing made of
//! the counter stays. Here `j = (i as number) + 3` stays an `i64` sum (the #2309 review's case,
//! where the rewrite ran before narrowing and `j` became a double). The unit test
//! `the_sign_tested_form_still_bounds_the_counter` covers later runs reading the form back.

mod no_window;
mod test_dir;

const SRC: &str = "function sumAt(a: f64[]): number {
  let s = 0;
  const n = a.length - 15;
  for (let i: usize = 0; i < n; i += (4 as usize)) {
    const j = (i as number) + 3;
    s += a[j];
  }
  return s;
}
function main() {
  const k = process.argv.length;
  const a: f64[] = [];
  for (let i = 0; i < k * 100; i++) {
    a.push(i);
  }
  console.log(sumAt(a));
}
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--release", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_counter_against_a_bound_that_may_be_negative_still_narrows_what_it_computes() {
    let vir = vir(SRC);
    // The exit test is in integers, with the sign test.
    assert!(vir.contains(" = bitand "), "no sign test:\n{vir}");
    // `j = i + 3` in integers, not `(i as f64) + 3.0`.
    assert!(vir.contains(", 3_i64"), "`j` is not an integer:\n{vir}");
    assert!(!vir.contains(", 3.0_f64"), "`j` is a double:\n{vir}");
}
