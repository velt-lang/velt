//! A key tested against a fixed list of names in the `any` glue (#2189): `Array.prototype`'s
//! and `Object.prototype`'s names in an array type's `AnyHas` and `AnyGet`, for example. Written
//! out, each list was one `str_eq` call and branch per name in every glue of every type with
//! it, about 166 blocks in an array type's `AnyHas` alone. It is now a static table and one
//! call of the shared `_Gkey_in`.
//!
//! The checks: those glue functions of an array type stay small, and `in` and reads through
//! `any` still answer as in Node for names on the lists, off them and as indexes.

mod no_window;
mod test_dir;

const SRC: &str = r#"const nums: number[] = [1, 2, 3];
const a: any = nums;
const o: any = { x: 1 };
const keys = ["push", "map", "length", "0", "3", "nope", "toString", "constructor", "x", "hasOwnProperty"];
const out: string[] = [];
for (const k of keys) {
  out.push(`${k}:${k in a}/${k in o}`);
}
out.push(`${a.nope === undefined} ${o.nope === undefined} ${a.length} ${o.x}`);
console.log(out.join(" "));
"#;

fn velt(args: &[&str]) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), SRC).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg("main.vlt")
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

/// Blocks of the function whose symbol starts with `prefix`, in `velt build --emit vir` output.
fn blocks_of(vir: &str, prefix: &str) -> usize {
    let start = vir
        .lines()
        .position(|l| {
            l.starts_with("fn#") && l.split_whitespace().nth(2).is_some_and(|s| s.starts_with(prefix))
        })
        .unwrap_or_else(|| panic!("no `{prefix}` in the VIR"));
    vir.lines()
        .skip(start + 1)
        .take_while(|l| *l != "}")
        .filter(|l| l.starts_with("  bb") && l.ends_with(':'))
        .count()
}

#[test]
fn an_array_types_any_glue_tests_the_prototype_names_with_one_call() {
    let vir = velt(&["build", "--emit", "vir"]);
    for glue in ["_Ganyhas_9f64_5b_5d(", "_Ganyget_9f64_5b_5d("] {
        let n = blocks_of(&vir, glue);
        // Written out, `Array.prototype`'s and `Object.prototype`'s names were about 166.
        assert!(n <= 40, "{glue}: {n} blocks");
    }
    assert!(vir.contains("_Gkey_in("), "no shared key test in the VIR");
}

#[test]
fn in_and_reads_through_any_answer_as_in_node() {
    // Node: node -e with the program above (types erased).
    let expect = "push:true/false map:true/false length:true/false 0:true/false 3:false/false \
                  nope:false/false toString:true/true constructor:true/true x:false/true \
                  hasOwnProperty:true/true true true 3 1\n";
    assert_eq!(velt(&["run"]), expect);
}
