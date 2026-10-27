//! Anyone can write `BTreeMap`: its sources (std/collections/btree_map.vlt and btree/*.vlt) use
//! only what any program can use, no std-only intrinsics or runtime declarations. The test
//! copies them out of std as plain modules of a program (imports made relative), and runs the
//! map's golden (tests/golden/std/btree_map.vlt) against the copy: a program may not use
//! `__intrinsic_*` or declare runtime functions, so it builds only if the map does neither, and
//! it prints the golden's output only if the copy is the same map.

use std::path::Path;

mod no_window;
mod test_dir;

/// `src` with the std module paths of the map's files made relative to a file in `here`
/// (`""` for the map's own directory, `"btree/"` for its parts).
fn relative(src: &str, here: &str) -> String {
    let parts = if here.is_empty() { "./btree/" } else { "./" };
    src.replace("\"velt:collections/btree/", &format!("\"{parts}"))
        .replace("\"velt:collections/btree_map\"", "\"./btree_map\"")
}

#[test]
fn the_btree_map_builds_and_runs_as_a_program_s_own_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = test_dir::TestDir::new();
    let parts = dir.path().join("btree");
    std::fs::create_dir_all(&parts).unwrap();
    let map = std::fs::read_to_string(root.join("std/collections/btree_map.vlt")).unwrap();
    std::fs::write(dir.path().join("btree_map.vlt"), relative(&map, "")).unwrap();
    for entry in std::fs::read_dir(root.join("std/collections/btree"))
        .unwrap()
        .flatten()
    {
        let src = std::fs::read_to_string(entry.path()).unwrap();
        std::fs::write(parts.join(entry.file_name()), relative(&src, "btree/")).unwrap();
    }
    let golden = std::fs::read_to_string(root.join("tests/golden/std/btree_map.vlt")).unwrap();
    let main = dir.path().join("main.vlt");
    std::fs::write(&main, relative(&golden, "")).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", main.to_str().unwrap()])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the copied map failed:\n{stderr}");
    let want = std::fs::read_to_string(root.join("tests/golden/std/btree_map.out")).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{stderr}");
}
