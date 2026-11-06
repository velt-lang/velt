//! Math operations are VIR statements (`Rvalue::Intrinsic`, #1981), not calls: `Math.sqrt`,
//! the roundings, `Math.clz32` and the integer bit counts lower to `_n = math.<op>(…)` in debug
//! and release builds, and no call of a `velt_rt_math_*` function is left for passes to see.

mod no_window;
mod test_dir;

const SRC: &str = "function f(x: number, n: u64): number {
  return Math.sqrt(x) + Math.floor(x) + Math.round(x) + Math.clz32(x) + (n.countOnes() as number);
}
const k = process.argv.length;
console.log(f(k + 1.5, (k as u64) * 255));
";

fn vir(release: bool) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), SRC).unwrap();
    let mut args = vec!["build", "main.velt", "--emit", "vir"];
    if release {
        args.push("--release");
    }
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
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
fn math_operations_are_statements() {
    for release in [false, true] {
        let v = vir(release);
        for op in ["sqrt", "clz32", "popcount64"] {
            assert!(
                v.contains(&format!("= math.{op}(")),
                "{op}, release: {release}\n{v}"
            );
        }
        assert!(
            !v.lines()
                .any(|l| l.contains("call extern") && l.contains("velt_rt_math_")),
            "a math call is left (release: {release})\n{v}"
        );
    }
}
