//! A module constant made by `new Atomic<T>(literal)` is a word in the program's data (velt_vir
//! `lower/static_atomic.rs`): its uses reach no module state, so they never take the module lock
//! (rt_abi.md `velt_rt_module_state`), which would serialize every task that uses the atomic.

use std::path::Path;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// The VIR of `src`, compiled as `main.vlt`.
fn vir(src: &str) -> String {
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

const BODY: &str = "
const HITS = new Atomic<u64>(INIT);

async function work() {
  HITS.add(1, \"relaxed\");
}

async function main() {
  await spawn(work());
  console.log(HITS.load());
}
";

#[test]
fn a_literal_starting_value_needs_no_module_state() {
    let v = vir(&BODY.replace("INIT", "0"));
    assert!(!v.contains("velt_rt_module_state"), "{v}");
    assert!(v.contains("velt_atomic_add_64_rlx"), "{v}");
}

#[test]
fn a_computed_starting_value_is_kept_in_the_module_state() {
    let src = format!(
        "function start(): u64 {{ return 2; }}\n{}",
        BODY.replace("INIT", "start()")
    );
    let v = vir(&src);
    assert!(v.contains("velt_rt_module_state"), "{v}");
}

/// A std module (std is copied with a probe added, used through `$VELT_STD`, as
/// gated_functions.rs does): its literal-start atomic is one word every call changes, and a
/// computed start, which std evaluates at each use, compiles too (it reached lowering's
/// static-atomic path and stopped with an `ICE:`).
#[test]
fn std_constants_with_literal_and_computed_starts() {
    let dir = test_dir::TestDir::new();
    let std = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    let probe = "function start(): u64 {\n  return 10;\n}\nconst ONE = new Atomic<u64>(0);\nconst EACH = new Atomic<u64>(start());\n\nexport function bump(): u64 {\n  return ONE.add(1) + 1;\n}\n\nexport function each(): u64 {\n  return EACH.add(1) + 1;\n}\n";
    std::fs::write(std.join("zz_static_atomic.vlt"), probe).unwrap();
    let main = "import { bump, each } from \"velt:zz_static_atomic\";\n\nfunction main() {\n  console.log(bump(), bump(), each());\n}\n";
    std::fs::write(dir.path().join("main.vlt"), main).unwrap();
    for release in [false, true] {
        let mut args = vec!["run"];
        if release {
            args.push("--release");
        }
        args.push("main.vlt");
        let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(&args)
            .env("VELT_STD", &std)
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "release={release}: {stderr}");
        assert_eq!(stdout, "1 2 11\n", "release={release}: {stderr}");
    }
}

/// The `mutable=<id>` of each writable static in the VIR of `src`.
fn word_ids(src: &str) -> Vec<String> {
    vir(src)
        .lines()
        .filter_map(|l| l.split(" mutable=").nth(1).map(str::to_string))
        .collect()
}

const COUNTER: &str = "
const HITS = new Atomic<u64>(0);

function main() {
  HITS.add(1);
  console.log(HITS.load());
}
";

/// A `velt dev` session keeps one word per id: the id stays when declarations come and go, and
/// changes with the type, so code of a version where the type changed never reads the old
/// word as the new type.
#[test]
fn word_ids_follow_name_and_type() {
    let before = word_ids(COUNTER);
    assert_eq!(before.len(), 1, "{before:?}");
    let more =
        format!("const limit: u64 = 3;\nfunction helper(): u64 {{ return limit; }}\n{COUNTER}");
    assert_eq!(before, word_ids(&more));
    for other in ["Atomic<u32>(0)", "Atomic<f64>(0)", "Atomic<i64>(0)"] {
        let changed = COUNTER.replace("Atomic<u64>(0)", other);
        assert_ne!(before, word_ids(&changed), "{other}");
    }
    assert_ne!(before, word_ids(&COUNTER.replace("HITS", "MISSES")));
}
