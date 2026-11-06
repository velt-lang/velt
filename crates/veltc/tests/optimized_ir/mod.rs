//! Reading the IR after clang's optimizer, for tests about code whose behavior no run can see: a
//! comparison or a select that must not become something else. Each test program is built for a
//! named target, so these run on every host whose clang has the target.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use veltc::cli::Emit;
use veltc::driver::{self, Artifact, BuildOptions, Session};

/// The program's IR for `target`, as the release backend gets it, with source locations.
pub fn release_ir(input: &Path, target: &str) -> String {
    let opts = BuildOptions {
        input: input.to_path_buf(),
        release: true,
        debug_info: true,
        emit: Emit::Llvm,
        target: Some(target.to_string()),
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::build(&mut sess, &opts) {
        Ok(Artifact::Llvm(ir)) => ir,
        Ok(_) => panic!("--emit llvm gave something other than IR"),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

/// `ir` after `clang`'s optimizer, or `None` when this clang doesn't have the target.
pub fn optimized(clang: &Path, ir: String, target: &str) -> Option<String> {
    let mut child = Command::new(clang)
        .args([
            velt_codegen_llvm::opt_flag(),
            "-S",
            "-emit-llvm",
            "-x",
            "ir",
        ])
        .args(["-Wno-override-module", &format!("--target={target}")])
        .args(["-", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run clang");
    let mut stdin = child.stdin.take().expect("clang's stdin");
    let writer = std::thread::spawn(move || stdin.write_all(ir.as_bytes()));
    let out = child.wait_with_output().expect("wait for clang");
    writer.join().expect("write the IR").expect("write the IR");
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() && err.contains("unable to create target") {
        return None;
    }
    assert!(out.status.success(), "clang failed for {target}:\n{err}");
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `%name`s on a line of IR.
pub fn values(line: &str) -> Vec<&str> {
    let mut found = vec![];
    let mut rest = line;
    while let Some(at) = rest.find('%') {
        rest = &rest[at + 1..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_'))
            .unwrap_or(rest.len());
        found.push(&rest[..end]);
        rest = &rest[end..];
    }
    found
}
