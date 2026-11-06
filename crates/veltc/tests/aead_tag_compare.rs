//! An AEAD's tag comparison stays one comparison in optimized code (`tagMatches`,
//! `std/crypto/aead_tag.velt`). A loop over the bytes behaves the same, so no program can see the
//! difference, but LLVM makes it 16 byte comparisons that branch at the first difference, and the
//! time of a forged tag's `open` then tells how many of its first bytes are right.
//!
//! So this reads the IR after clang's optimizer, for each AEAD alone (the comparison is inlined
//! into its one caller, where the optimizer does most) and each target: in the code that debug
//! info attributes to `aead_tag.velt`, whatever is loaded through an array's data pointer reaches
//! exactly one `icmp`, and no branch or select. Code generation after the IR is not seen here:
//! the check for that, on x86-64, is the memcheck command in `aead_tag.velt`.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use veltc::cli::Emit;
use veltc::driver::{self, Artifact, BuildOptions, Session};

/// The program's IR for `target`, as the release backend gets it, with source locations.
fn release_ir(program: &str, target: &str) -> String {
    let opts = BuildOptions {
        input: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/aead_tag_compare")
            .join(program),
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
fn optimized(clang: &Path, ir: String, target: &str) -> Option<String> {
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

/// The `!n` nodes whose scope lies in `aead_tag.velt`: its functions, or their code inlined.
fn compare_locations(ir: &str) -> HashSet<&str> {
    let nodes: Vec<(&str, &str)> = ir
        .lines()
        .filter(|l| l.starts_with('!'))
        .filter_map(|l| l.split_once(" = "))
        .collect();
    let file = nodes
        .iter()
        .find(|(_, n)| n.contains("DIFile(") && n.contains("aead_tag.velt\""))
        .map(|(id, _)| format!("file: {id},"));
    let in_file = |n: &str| file.as_ref().is_some_and(|f| n.contains(f.as_str()));
    let mut inside: HashSet<&str> = nodes
        .iter()
        .filter(|(_, n)| {
            (n.contains("DISubprogram(") || n.contains("DILexicalBlockFile(")) && in_file(n)
        })
        .map(|(id, _)| *id)
        .collect();
    loop {
        let before = inside.len();
        for (id, node) in &nodes {
            let scope = node.split("scope: ").nth(1);
            let scope = scope.and_then(|s| s.split([',', ')']).next());
            let located = node.contains("DILocation(") || node.contains("DILexicalBlock");
            if located && scope.is_some_and(|s| inside.contains(s)) {
                inside.insert(id);
            }
        }
        if inside.len() == before {
            return inside;
        }
    }
}

/// The `%name`s on a line of IR.
fn values(line: &str) -> Vec<&str> {
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

/// What the compared bytes reach in one function: their `icmp`s, and their branches and selects.
fn secret_uses(function: &str, inside: &HashSet<&str>) -> (Vec<String>, Vec<String>) {
    let ours = |l: &str| {
        l.split("!dbg ")
            .nth(1)
            .and_then(|d| d.split([',', ' ']).next())
            .is_some_and(|d| inside.contains(d))
    };
    let lines: Vec<&str> = function.lines().map(str::trim).collect();
    // The arrays' data pointers, the values loaded through them, and bytes or vectors loaded by
    // this code (their pointer can be loaded by the caller). Lengths are `i64`s in the array's
    // header, which is not behind such a pointer.
    let mut pointers: HashSet<&str> = HashSet::new();
    let mut secrets: HashSet<&str> = HashSet::new();
    loop {
        let before = pointers.len() + secrets.len();
        for l in &lines {
            let Some((result, op)) = l.split_once(" = ") else {
                continue;
            };
            let result = result.trim_start_matches('%');
            let used = values(op);
            if op.starts_with("load ptr") && ours(l) {
                pointers.insert(result);
            } else if op.starts_with("load ") {
                let bytes = ["load i8", "load <", "load i128"];
                let data = ours(l) && bytes.iter().any(|b| op.starts_with(b));
                if data || used.iter().any(|v| pointers.contains(v)) {
                    secrets.insert(result);
                }
            } else if used.iter().any(|v| pointers.contains(v)) {
                pointers.insert(result);
            } else if used.iter().any(|v| secrets.contains(v)) {
                secrets.insert(result);
            }
        }
        if pointers.len() + secrets.len() == before {
            break;
        }
    }
    let reached: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| ours(l) && values(l).iter().any(|v| secrets.contains(v)))
        .collect();
    let of = |wanted: &dyn Fn(&str) -> bool| -> Vec<String> {
        let found = reached.iter().filter(|l| wanted(l));
        found.map(|l| l.to_string()).collect()
    };
    (
        of(&|l| l.contains(" = icmp ")),
        of(&|l| l.starts_with("br ") || l.starts_with("switch ") || l.contains(" = select ")),
    )
}

#[test]
fn the_tag_is_compared_once_without_a_branch() {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let host = velt_codegen_llvm::host_triple();
    let mut checked = 0;
    for target in [host.as_str(), "aarch64-unknown-linux-gnu", "wasm32-wasip1"] {
        for program in ["chacha.velt", "aes_gcm.velt"] {
            let Some(ir) = optimized(&clang, release_ir(program, target), target) else {
                eprintln!("note: this clang has no {target}; skipping it");
                continue;
            };
            let inside = compare_locations(&ir);
            let (mut compares, mut branches) = (vec![], vec![]);
            for function in ir.split("\ndefine ").skip(1) {
                let (c, b) = secret_uses(function, &inside);
                compares.extend(c);
                branches.extend(b);
            }
            assert!(
                branches.is_empty(),
                "{program}, {target}: the tag's bytes reach a branch:\n{}",
                branches.join("\n")
            );
            assert_eq!(
                compares.len(),
                1,
                "{program}, {target}: the tag's bytes should reach one comparison:\n{}",
                compares.join("\n")
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "clang has none of the targets");
}
