//! `console.log` of a whole `number` in a release build (#2197): `velt_opt` numrep calls the
//! integer writer `intWrite` in place of the float writer `numberWrite` where it proves the number
//! whole and not `-0`, but only where the program has `intWrite`. `numberWrite` holds no integer
//! printing, so a program that prints only `number`s has none, and keeps the float writer (the
//! runtime's printer: golden `console_log_floats_alone`).

mod no_window;
mod test_dir;

/// The release VIR of `src`, compiled as `main.velt`.
fn vir_of(src: &str) -> String {
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

/// The lines of `vir` that call something whose name contains `name`.
fn calls(vir: &str, name: &str) -> usize {
    vir.lines()
        .filter(|l| l.trim_start().starts_with("call ") && l.contains(name))
        .count()
}

/// A whole `number` (`i`) and an `i64` (`n`) printed. The writers are inlined into `main` by the
/// time the VIR is written: `intWrite`'s call is one of `digitsWrite`, `numberWrite`'s one of
/// the runtime's `velt_rt_write_f64`.
const BOTH: &str = "function main() {
  for (let i = 0; i < 1000; i++) {
    console.log(i);
  }
  const n: i64 = process.argv.length as i64;
  console.log(n);
}
";

#[test]
fn a_whole_number_takes_the_integer_writer_where_the_program_has_it() {
    let vir = vir_of(BOTH);
    assert_eq!(calls(&vir, "velt_rt_write_f64"), 0, "{vir}");
    assert_eq!(calls(&vir, "digitsWrite("), 2, "{vir}");
}

#[test]
fn a_program_that_prints_only_numbers_keeps_the_float_writer() {
    let src = BOTH.replace("console.log(n);", "const m = n + 1;\n  console.log(0.5, m > 0);");
    let vir = vir_of(&src);
    assert_eq!(calls(&vir, "velt_rt_write_f64"), 2, "{vir}");
    assert_eq!(calls(&vir, "Write("), 0, "{vir}");
}
