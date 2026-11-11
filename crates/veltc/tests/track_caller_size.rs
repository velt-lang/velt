//! A large standard-library function that reports its caller's location when it panics gets one
//! instance for all call sites, taking the location as a hidden argument (velt_vir
//! track_caller.rs): a program's code does not grow by a copy of the function's whole call tree
//! per call site. `BTreeMap.set` reaches `insert`, which can panic through the checked array
//! methods it uses.

mod no_window;
mod test_dir;

#[test]
fn a_large_caller_tracking_function_has_one_instance_for_all_call_sites() {
    let dir = test_dir::TestDir::new();
    let main = dir.path().join("main.velt");
    std::fs::write(
        &main,
        "import { BTreeMap } from \"velt/collections\";\n\
         function main() {\n\
           const m = new BTreeMap<string, i64>();\n\
           m.set(\"a\", 1);\n\
           m.set(\"b\", 2);\n\
           m.set(\"c\", 3);\n\
           console.log(m.size);\n\
         }\n",
    )
    .unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--emit", "vir", main.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    let inserts: Vec<&str> = vir
        .lines()
        .filter(|l| l.starts_with("fn#") && l.contains("btreeP6insertN6insert_T"))
        .collect();
    assert_eq!(inserts.len(), 1, "{inserts:#?}");
    assert!(inserts[0].contains("_LA("), "{}", inserts[0]);
}

#[test]
fn an_internal_error_does_not_track_the_caller() {
    // `Uint8Array.subarray` can only panic with an internal error (`panic(`ICE: …`)`): that is
    // std's bug, reported where it is, so the method keeps one instance with no location.
    let dir = test_dir::TestDir::new();
    let main = dir.path().join("main.velt");
    std::fs::write(
        &main,
        "function main() {\n\
           const b = new Uint8Array([1, 2, 3]);\n\
           console.log(b.subarray(1, 2).length, b.subarray(0).length);\n\
         }\n",
    )
    .unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--emit", "vir", main.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    let subarrays: Vec<&str> = vir
        .lines()
        .filter(|l| l.starts_with("fn#") && l.contains("M8subarray"))
        .collect();
    assert_eq!(subarrays.len(), 1, "{subarrays:#?}");
    assert!(!subarrays[0].contains("_L"), "{}", subarrays[0]);
}

/// The VIR lines of `Map.set` instances in `--emit vir` of three calls from three sites.
fn map_set_instances(env: &[(&str, &str)]) -> Vec<String> {
    let dir = test_dir::TestDir::new();
    let main = dir.path().join("main.velt");
    std::fs::write(
        &main,
        "function main() {\n\
           const m = new Map<string, i64>();\n\
           m.set(\"a\", 1);\n\
           m.set(\"b\", 2);\n\
           m.set(\"c\", 3);\n\
           console.log(m.size);\n\
         }\n",
    )
    .unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "--emit", "vir", main.to_str().unwrap()]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("fn#") && l.contains("MapM3set_T"))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_debug_build_passes_every_caller_location_as_an_argument() {
    // `Map.set` is small enough for a copy per call site; a debug build, which inlines none of
    // them, compiles one instance taking the location instead (#2707).
    let sets = map_set_instances(&[]);
    assert_eq!(sets.len(), 1, "{sets:#?}");
    assert!(sets[0].contains("_LA("), "{}", sets[0]);
    // Builds that don't share instances keep a copy per call site (the constructor's too).
    let per_site = map_set_instances(&[("VELT_SHAPES", "off")]);
    assert!(per_site.len() >= 3, "{per_site:#?}");
}
