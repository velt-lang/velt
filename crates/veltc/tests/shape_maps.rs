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

/// Maps whose key types share a shape (`string | number`, `string | i64`) but hash and compare
/// per member: NaN equals NaN, -0 equals 0 for a `number`, and an `i64` compares by value.
const UNION_KEYS: &str = "
function main() {
  const mn = new Map<string | number, string>();
  mn.set(NaN, \"nan\");
  mn.set(-1, \"neg\");
  mn.set(0, \"zero\");
  mn.set(\"k\", \"s\");
  const mi = new Map<string | i64, string>();
  // -1 and -2 are both NaN bit patterns: as `number` keys they would be one key.
  const m1: i64 = -1;
  const m2: i64 = -2;
  mi.set(m1, \"i-neg\");
  mi.set(m2, \"i-neg2\");
  mi.set(\"k\", \"t\");
  console.log(mn.get(NaN), mn.get(-1), mn.get(-0), mn.get(\"k\"), mi.get(m1), mi.get(m2), mi.get(\"k\"), mn.size, mi.size);
}
";

#[test]
fn shared_map_code_hashes_and_compares_each_key_type_its_own_way() {
    // One shared instance (its name carries the shape, `~`, mangled `_7e`) ...
    let shared = map_sets(UNION_KEYS, &[]);
    assert_eq!(shared.len(), 1, "{shared:#?}");
    assert!(shared[0].contains("_7e"), "{}", shared[0]);
    // ... that reaches each key type's own hash and comparison through its dictionary.
    let want = "nan neg zero s i-neg i-neg2 t 4 3\n";
    for env in [&[][..], &[("VELT_SHAPES", "off")][..]] {
        let (ok, out, err) = velt(UNION_KEYS, &["run"], env);
        assert!(ok, "{err}");
        assert_eq!(out, want, "{env:?}");
    }
}

#[test]
fn a_shape_of_one_type_is_not_shared() {
    // A dictionary gains nothing with one member and costs its indirect calls: after the first
    // pass, a pair whose shape had one member keeps its per-type instance.
    let one = "
type A = { a: number; s: string };
function main() {
  const ma = new Map<string, A>();
  ma.set(\"x\", { a: 1, s: \"one\" });
  ma.set(\"y\", { a: 2, s: \"two\" });
  console.log(ma.size);
}
";
    let sets = map_sets(one, &[]);
    assert_eq!(sets.len(), 1, "{sets:#?}");
    assert!(!sets[0].contains("_7e"), "{}", sets[0]);
    // Two record types of one layout do share.
    let two = map_sets(TWO_MAPS, &[]);
    assert!(two[0].contains("_7e"), "{}", two[0]);
}
