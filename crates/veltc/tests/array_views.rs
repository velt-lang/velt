//! Array views (#1485, velt_vir `lower/array_view.rs`): an array of another element type
//! converted to an array type is a view of that array. Only the array types such a conversion
//! targets carry the view code; a program without one has none of it, and the other array
//! types of a program with one are unchanged.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
fn vir_of(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
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

/// The number of calls in `vir` of functions whose name contains `name`.
fn calls(vir: &str, name: &str) -> usize {
    vir.lines()
        .filter(|l| !l.starts_with("fn#") && l.contains("call fn#") && l.contains(name))
        .count()
}

const PLAIN: &str = "
const xs: number[] = [1, 2, 3];
let n = 0;
for (const x of xs) {
  n += x;
}
console.log(n, xs.length);
";

#[test]
fn a_conversion_to_an_array_type_makes_views() {
    let src = format!("const kids: any = [\"a\", 1];\nconst v = kids as (string | number)[];\nconsole.log(v.length);\n{PLAIN}");
    let vir = vir_of(&src);
    assert!(
        calls(&vir, velt_common::std_hook_name!(array_view_new)) > 0,
        "no call of `arrayViewNew`"
    );
    assert!(
        calls(&vir, velt_common::std_hook_name!(array_view_sync)) > 0,
        "no sync of a view"
    );
}

#[test]
fn a_program_without_one_has_no_view_code() {
    let vir = vir_of(PLAIN);
    assert_eq!(calls(&vir, "arrayView"), 0, "view code without a view");
}

#[test]
fn other_array_types_are_unchanged() {
    // `number[]` is no view type, so its loop and `length` sync nothing: the only syncs are
    // those of the view type's own accesses (in `main` and the prelude's view functions).
    let with = vir_of(&format!(
        "const kids: any = [\"a\"];\nconst v = kids as string[];\nconsole.log(v.length);\n{PLAIN}"
    ));
    let alone =
        vir_of("const kids: any = [\"a\"];\nconst v = kids as string[];\nconsole.log(v.length);\n");
    assert_eq!(
        calls(&with, velt_common::std_hook_name!(array_view_sync)),
        calls(&alone, velt_common::std_hook_name!(array_view_sync))
    );
}

#[test]
fn a_loop_runs_its_original_for_a_view_through_a_cold_call() {
    // The loop of `sum` is versioned for the box of `xs` (a parameter the loop does not
    // change): the test before it calls `velt_rt_array_view_cold` on the way into the original
    // (for a view), once, and the copy for a plain array calls nothing.
    let vir = vir_of(
        "function sum(xs: string[]): number {
  let n = 0;
  for (let i = 0; i < xs.length; i++) {
    n += xs[i].length;
  }
  return n;
}
const kids: any = [\"a\", \"bc\"];
console.log(sum(kids as string[]), sum([\"x\"]));
",
    );
    let sum: Vec<&str> = vir
        .split("\nfn#")
        .filter(|f| f.lines().next().is_some_and(|l| l.contains("sum(")))
        .collect();
    assert_eq!(sum.len(), 1, "no single `sum` in the VIR:\n{vir}");
    let cold = sum[0]
        .lines()
        .filter(|l| l.contains("call extern#") && l.contains("velt_rt_array_view_cold"))
        .count();
    assert_eq!(cold, 1, "{}", sum[0]);
}

#[test]
fn a_loop_copy_reads_past_the_length_of_a_row_its_test_found_plain_as_a_plain_array() {
    // In the copy of the loop of `total`, `rows[k][i]` reads the row whose header the loop's
    // test `i < rows[k].length` found plain, with nothing in between that may write memory: past
    // its length is the plain array's out-of-bounds error, so the copy goes on in its own access
    // past the length (which takes the header's address, as a view's would its mirror's)
    // instead of in the original's. The function has two of them, the original's and the copy's.
    let vir = vir_of(
        "function total(rows: string[][]): number {
  let n = 0;
  for (let k = 0; k < rows.length; k++) {
    for (let i = 0; i < rows[k].length; i++) {
      n += rows[k][i].length;
    }
  }
  return n;
}
const kids: any = [1, \"bc\"];
console.log(total([kids as string[], [\"x\"]]), total([[\"y\", \"zz\"]]));
",
    );
    let total: Vec<&str> = vir
        .split(
            "
fn#",
        )
        .filter(|f| f.lines().next().is_some_and(|l| l.contains("total(")))
        .collect();
    assert_eq!(
        total.len(),
        1,
        "no single `total` in the VIR:
{vir}"
    );
    let past = total[0].lines().filter(|l| l.contains("= &(*(*")).count();
    assert_eq!(past, 2, "{}", total[0]);
}

/// The lines of the function in `vir` whose signature line contains `name`.
fn fn_lines<'a>(vir: &'a str, name: &str) -> Vec<&'a str> {
    let fns: Vec<&str> = vir
        .split("\nfn#")
        .filter(|f| f.lines().next().is_some_and(|l| l.contains(name)))
        .collect();
    assert_eq!(fns.len(), 1, "no single `{name}` in the VIR:\n{vir}");
    fns[0].lines().collect()
}

/// Is `rhs` a copy of a whole array header held in a box (`(*_3 as agg#5)`)?
fn whole_header_copy(rhs: &str) -> bool {
    rhs.starts_with("(*_") && rhs.ends_with(')') && rhs.contains(" as agg#")
}

#[test]
fn a_runtime_function_gets_an_array_of_a_view_type_in_place() {
    // #1831: `i64[]` is a view type here, so its arrays are boxed. `RegExp.execAt` passes its
    // offsets array to `velt_rt_regex_exec`, which writes the header (a new buffer and length):
    // the extern gets the box's own header, not a copy of it in a temporary (whose writes would
    // be lost), and the view's mirror is written back after the call.
    let vir = vir_of(
        "const ns: any = [1];\nconst is = ns as i64[];\nconsole.log(is.length, /a/.exec(\"ba\")?.index);\n",
    );
    let exec = fn_lines(&vir, "6RegExpM6execAt(");
    let call = exec
        .iter()
        .position(|l| l.contains("call extern#") && l.contains("velt_rt_regex_exec("))
        .unwrap_or_else(|| panic!("no call of `velt_rt_regex_exec`:\n{}", exec.join("\n")));
    let arg = exec[call]
        .rsplit_once(", ")
        .and_then(|(_, a)| a.split_once(')'))
        .map(|(a, _)| a.trim())
        .unwrap_or_else(|| panic!("cannot read the last argument of {}", exec[call]));
    let def = |local: &str| -> Vec<&str> {
        let lhs = format!("{local} = ");
        (exec.iter())
            .filter_map(|l| l.trim().strip_prefix(lhs.as_str()))
            .collect()
    };
    for rhs in def(arg) {
        if let Some(tmp) = rhs.strip_prefix('&') {
            assert!(
                !def(tmp).into_iter().any(whole_header_copy),
                "the offsets argument `{arg}` is a copy of the header:\n{}",
                exec.join("\n")
            );
        }
    }
    let flush = exec[call..]
        .iter()
        .any(|l| l.contains("call fn#") && l.contains("arrayViewFlush"));
    assert!(
        flush,
        "no write-back of a view after the call:\n{}",
        exec.join("\n")
    );
}

#[test]
fn a_standard_library_class_has_no_any_glue_for_its_private_methods() {
    // #1831: a `RegExp` method called through `any` builds the class's call glue, which once
    // had a case for the private `matchAt(s, offsets: i64[], at)` too, converting an argument to
    // `i64[]` (so making it a view type for the whole program). No program can name it.
    let vir = vir_of("const a: any = /b/;\nconsole.log(a.test(\"abc\"));\n");
    let views = vir
        .lines()
        .filter(|l| l.starts_with("fn#") && l.contains("arrayView") && l.contains("_T3i64"))
        .count();
    assert_eq!(views, 0, "`i64[]` is a view type");
}
