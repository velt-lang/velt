//! The prelude `Map` family shared by shape in debug builds (velt_vir `lower/shape/`, step 4 of
//! docs/internals/design/shape-sharing.md, #2707): maps of two record types with the same layout
//! share one instance of each `<K, V>` function, with the same output.

mod no_window;
mod test_dir;

/// Two maps whose values have one layout (a number and a string), set and read.
const TWO_MAPS: &str = "
type A = { a: number; s: string };
type B = { b: number; t: string };
function main() {
  const ma = new Map<string, A>();
  const mb = new Map<string, B>();
  ma.set(\"x\", { a: 1, s: \"one\" });
  mb.set(\"y\", { b: 2, t: \"two\" });
  ma.set(\"z\", { a: 3, s: \"three\" });
  console.log(ma.get(\"x\")!.s, mb.get(\"y\")!.b, ma.get(\"z\")!.a, ma.size + mb.size);
}
";

/// `velt <args> main.velt` of `src` with `env`: (success, stdout, stderr).
fn velt(src: &str, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args).arg("main.velt").current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `Map.set` instances in a debug build's VIR of `src`.
fn map_sets(src: &str, env: &[(&str, &str)]) -> Vec<String> {
    let (ok, vir, err) = velt(src, &["build", "--emit", "vir"], env);
    assert!(ok, "{err}");
    vir.lines()
        .filter(|l| l.starts_with("fn#") && l.contains("MapM3set_T"))
        .map(str::to_string)
        .collect()
}

#[test]
fn maps_of_one_layout_share_their_instances() {
    let shared = map_sets(TWO_MAPS, &[]);
    assert_eq!(shared.len(), 1, "{shared:#?}");
    let per_type = map_sets(TWO_MAPS, &[("VELT_SHAPES", "off")]);
    assert!(per_type.len() >= 2, "{per_type:#?}");
}

#[test]
fn shared_map_code_prints_what_per_type_code_prints() {
    let want = "one 2 3 3\n";
    for env in [&[][..], &[("VELT_SHAPES", "off")][..]] {
        let (ok, out, err) = velt(TWO_MAPS, &["run"], env);
        assert!(ok, "{err}");
        assert_eq!(out, want, "{env:?}");
    }
}
