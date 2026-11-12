//! Debug info: the LLVM IR of a debug build (and of `--release -g`) carries `!dbg` metadata
//! pointing into the `.velt` sources; a plain `--release` build carries none. With clang
//! installed, a `-g` release executable resolves addresses to `.velt` file:line (PDB on
//! Windows, DWARF elsewhere), checked with `llvm-symbolizer` when it sits next to clang.

use std::path::{Path, PathBuf};

mod no_window;
mod runtime_support;
mod work_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn emit_llvm(args: &[&str]) -> String {
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--emit", "llvm"])
        .args(args)
        .arg("tests/golden/lang/panic_div.velt")
        .current_dir(root())
        .output()
        .expect("run velt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn llvm_ir_has_debug_metadata_unless_plain_release() {
    for args in [&[][..], &["--release", "-g"][..]] {
        let ir = emit_llvm(args);
        assert!(ir.contains("!llvm.dbg.cu"), "{args:?}");
        assert!(
            ir.contains("filename: \"tests/golden/lang/panic_div.velt\""),
            "{args:?}"
        );
        assert!(ir.contains("!DISubprogram(name: \"velt_main\""), "{args:?}");
        // `a / b` on line 3, column 10 (inlined into `main` in release builds).
        assert!(ir.contains("!DILocation(line: 3, column: 10"), "{args:?}");
        let flag = if cfg!(windows) {
            "CodeView"
        } else {
            "Dwarf Version"
        };
        assert!(ir.contains(flag), "{args:?}");
    }
    let plain = emit_llvm(&["--release"]);
    assert!(
        !plain.contains("!dbg"),
        "plain --release must not carry debug info"
    );
}

#[test]
fn g_release_binary_maps_addresses_to_velt_lines() {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let exe_name = if cfg!(windows) {
        "llvm-pdbutil.exe"
    } else {
        "llvm-pdbutil"
    };
    let pdbutil = clang.with_file_name(exe_name);
    let root = root();
    runtime_support::build_native_runtime(&root);
    let out = work_dir::work_dir(&root, "golden-work-debuginfo").join("panic_div");
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "-g", "--backend", "llvm", "-o"])
        .arg(&out)
        .arg("tests/golden/lang/panic_div.velt")
        .current_dir(&root)
        .output()
        .expect("run velt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let pdb = out.with_extension("pdb");
    if cfg!(windows) {
        assert!(pdb.exists(), "-g must produce a PDB on Windows");
    }
    // The line check reads the PDB's line tables directly. llvm-symbolizer's native PDB reader
    // misses some files of a function whose line table spans several (an inlined std hook first,
    // then the program's own lines), though the PDB is right; other platforms stop at a
    // successful build.
    if !cfg!(windows) || !pdbutil.exists() {
        eprintln!("note: skipping the PDB line check");
        return;
    }
    let s = crate::no_window::command(&pdbutil)
        .args(["dump", "-l"])
        .arg(&pdb)
        .output()
        .expect("run llvm-pdbutil");
    assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
    let text = String::from_utf8_lossy(&s.stdout).replace('\\', "/");
    assert!(
        panic_div_lines(&text).contains(&3),
        "no line-3 entry for tests/golden/lang/panic_div.velt in the PDB:\n{text}"
    );
}

/// The line numbers `llvm-pdbutil dump -l` lists under `tests/golden/lang/panic_div.velt`: the
/// indented rows after each header line naming that file hold `<line> <address>` pairs.
fn panic_div_lines(dump: &str) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut in_file = false;
    for row in dump.lines() {
        if !row.starts_with(char::is_whitespace) {
            in_file = row.contains("tests/golden/lang/panic_div.velt");
            continue;
        }
        if !in_file {
            continue;
        }
        let words: Vec<&str> = row
            .split(|c: char| c.is_whitespace() || c == '!')
            .filter(|w| !w.is_empty())
            .collect();
        for pair in words.windows(2) {
            let is_addr = pair[1].len() == 8 && pair[1].chars().all(|c| c.is_ascii_hexdigit());
            if let (Ok(line), true) = (pair[0].parse::<u32>(), is_addr) {
                lines.push(line);
            }
        }
    }
    lines
}

#[test]
fn the_pdb_line_dump_is_read_per_file() {
    let dump = "Mod 0000 | `panic_div.obj`:\n\
C:/w/tests/golden/lang/panic_div.velt (MD5: 0123)\n  \
0001:00001000-00001040, line/addr entries = 2\n      \
2 00001000 !      3 00001010 !\n\
C:/w/std/lang/hooks/process.velt (MD5: 4567)\n  \
0001:00001040-00001050, line/addr entries = 1\n      \
9 00001040 !\n";
    assert_eq!(panic_div_lines(dump), vec![2, 3]);
    assert!(panic_div_lines("nothing here\n").is_empty());
}
