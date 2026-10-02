//! Debug info: the LLVM IR of a debug build (and of `--release -g`) carries `!dbg` metadata
//! pointing into the `.vlt` sources; a plain `--release` build carries none. With clang
//! installed, a `-g` release executable resolves addresses to `.vlt` file:line (PDB on
//! Windows, DWARF elsewhere), checked with `llvm-symbolizer` when it sits next to clang.

use std::path::{Path, PathBuf};
use std::process::Command;

mod runtime_support;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn emit_llvm(args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--emit", "llvm"])
        .args(args)
        .arg("tests/golden/lang/panic_div.vlt")
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
            ir.contains("filename: \"tests/golden/lang/panic_div.vlt\""),
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
        "llvm-symbolizer.exe"
    } else {
        "llvm-symbolizer"
    };
    let symbolizer = clang.with_file_name(exe_name);
    let root = root();
    runtime_support::build_native_runtime(&root);
    let out = root.join("target/golden-work-debuginfo/panic_div");
    let o = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "-g", "--backend", "llvm", "-o"])
        .arg(&out)
        .arg("tests/golden/lang/panic_div.vlt")
        .current_dir(&root)
        .output()
        .expect("run velt");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let exe = if cfg!(windows) {
        assert!(
            out.with_extension("pdb").exists(),
            "-g must produce a PDB on Windows"
        );
        out.with_extension("exe")
    } else {
        out
    };
    // The address check assumes link.exe's default layout (image base 0x140000000, `.text`
    // first, the program's object first in it); other platforms stop at a successful build.
    if !cfg!(windows) || !symbolizer.exists() {
        eprintln!("note: skipping the llvm-symbolizer line check");
        return;
    }
    let addrs: Vec<String> = (0..64u64)
        .map(|i| format!("{:#x}", 0x1_4000_1000 + i * 4))
        .collect();
    let s = Command::new(&symbolizer)
        .arg(format!("--obj={}", exe.display()))
        .args(&addrs)
        .output()
        .expect("run llvm-symbolizer");
    let text = String::from_utf8_lossy(&s.stdout).replace('\\', "/");
    assert!(
        text.contains("tests/golden/lang/panic_div.vlt:"),
        "no .vlt line info:\n{text}"
    );
}
