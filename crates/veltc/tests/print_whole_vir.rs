//! `console.log` of a whole `number` in a release build (#2197): `velt_opt` numrep calls a twin of
//! the float writer `numberWrite` where it proves the number whole, below 2^53 and not `-0`. The
//! twin is the integer writer `intWrite` where the program has it, and else `wholeNumberWrite`,
//! `numberWrite`'s own (std/lang/hooks/number_write): `numberWrite` holds no integer printing, so
//! a program that prints only `number`s has no `intWrite`.

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

/// The body of `velt_main` in `vir`, where the writers' calls are once inlined (the writers'
/// own bodies call the runtime's printer too).
fn main_body(vir: &str) -> &str {
    let start = vir
        .find("export velt_main(")
        .unwrap_or_else(|| panic!("no velt_main:\n{vir}"));
    let rest = &vir[start..];
    rest.find("\nfn#").map_or(rest, |end| &rest[..end])
}

/// The JS loop: a whole `number` (`i`) printed, and nothing else.
const LOOP: &str = "function main() {
  for (let i = 0; i < 1000; i++) {
    console.log(i);
  }
}
";

/// The JS loop and an `i64` (`n`) printed. The writers are inlined into `main` by the time the
/// VIR is written: `intWrite`'s call is one of `digitsWrite`.
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
    let main = main_body(&vir);
    assert_eq!(calls(main, "velt_rt_write_f64"), 0, "{vir}");
    assert_eq!(calls(&vir, "wholeNumberWrite("), 0, "{vir}");
    assert_eq!(calls(main, "digitsWrite("), 2, "{vir}");
}

#[test]
fn the_js_loop_alone_takes_the_whole_number_writer() {
    let vir = vir_of(LOOP);
    let main = main_body(&vir);
    assert_eq!(calls(main, "velt_rt_write_f64"), 0, "{vir}");
    assert_eq!(calls(main, "wholeNumberWrite("), 1, "{vir}");
    assert_eq!(calls(&vir, "digitsWrite("), 0, "{vir}");
}
