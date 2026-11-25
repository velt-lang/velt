//! A function that returns a promise, held as `any` (#924 step 3): its apply slot calls it only in
//! a program that calls a value held as `any` (velt_vir `Boxing::any_applies`). A program that
//! only reads such functions' names would otherwise build the promise's `any` table, its glue and
//! the executor that glue reaches, for a call it never makes.
//!
//! The check: the VIR of a program holding an async function as `any` has no glue of a promise's
//! `any` table (`_Ganyawait_`) until the program calls a value through `any`.

mod no_window;
mod test_dir;

const HELD: &str = "async function load(): Promise<number> {
  return 1;
}
const kinds: any[] = [load];
console.log(kinds[0].name);
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
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

fn promise_glue(v: &str) -> bool {
    v.lines()
        .any(|l| l.starts_with("fn#") && l.contains(" _Ganyawait_"))
}

#[test]
fn an_async_function_held_as_any_is_called_from_glue_only_where_the_program_calls_an_any() {
    assert!(
        !promise_glue(&vir(HELD)),
        "a program that calls no value held as `any` has a promise's `any` glue"
    );
    let calls = format!("{HELD}const f: any = (n: number) => n;\nconsole.log(f(1));\n");
    assert!(
        promise_glue(&vir(&calls)),
        "a program that calls a value held as `any` has no promise's `any` glue: the check above tests nothing"
    );
}
