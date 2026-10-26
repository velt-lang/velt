//! `tests/golden/lang/opt_number_arrays_whole.vlt` and `opt_number_arrays_doubles.vlt` check
//! that the output stays Node's; this checks that a release build really stores the first
//! file's arrays as integers (#1599) and keeps the second file's as doubles.

use std::collections::HashSet;
use std::path::Path;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The release VIR of a golden program.
fn release_vir(golden: &str) -> String {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden")
        .join(golden);
    let opts = BuildOptions {
        input,
        release: true,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

/// Each function of `vir`: its header and body lines.
fn functions(vir: &str) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = vec![];
    for line in vir.lines() {
        if line.starts_with("fn#") {
            out.push((line, vec![]));
        } else if let Some((_, body)) = out.last_mut() {
            body.push(line);
        }
    }
    out
}

/// The element loads and stores of type `ty` (`f64` or `i64`) in `body` through the data
/// pointer of one of the local arrays `arrays` (the local `sroa` names `<array>.0`), or a
/// copy or offset of it.
fn element_accesses(body: &[&str], arrays: &[&str], ty: &str) -> usize {
    let mut ptrs: HashSet<String> = HashSet::new();
    for line in body {
        // `  let _33: ptr // xs.0`
        let Some(rest) = line.trim().strip_prefix("let ") else {
            continue;
        };
        let Some((local, decl)) = rest.split_once(": ptr // ") else {
            continue;
        };
        if arrays.iter().any(|a| decl.trim() == format!("{a}.0")) {
            ptrs.insert(local.to_string());
        }
    }
    // `_5 = ptradd _33, _4` and `_7 = _33` reach the same elements.
    loop {
        let before = ptrs.len();
        for line in body {
            let Some((dst, src)) = line.trim().split_once(" = ") else {
                continue;
            };
            let base = src
                .strip_prefix("ptradd ")
                .map_or(src, |s| s.split(',').next().unwrap_or(s))
                .trim();
            if ptrs.contains(base) && dst.starts_with('_') && !dst.contains(' ') {
                ptrs.insert(dst.to_string());
            }
        }
        if ptrs.len() == before {
            break;
        }
    }
    ptrs.iter()
        .map(|p| {
            let access = format!("(*{p} as {ty})");
            body.iter()
                .map(|l| l.matches(&access).count())
                .sum::<usize>()
        })
        .sum()
}

#[test]
fn whole_number_arrays_hold_integers() {
    let vir = release_vir("lang/opt_number_arrays_whole.vlt");
    let arrays = ["perm", "perm1", "xs"];
    let fns = functions(&vir);
    let ints: usize = fns
        .iter()
        .map(|(_, b)| element_accesses(b, &arrays, "i64"))
        .sum();
    assert!(ints > 0, "no integer element access\n{vir}");
    for (header, body) in &fns {
        assert_eq!(
            element_accesses(body, &arrays, "f64"),
            0,
            "{header} still reads or writes double elements\n{}",
            body.join("\n")
        );
    }
}

#[test]
fn arrays_that_may_hold_other_numbers_keep_doubles() {
    let vir = release_vir("lang/opt_number_arrays_doubles.vlt");
    let arrays = ["fractions", "zeros", "prev", "curr", "count", "big"];
    let fns = functions(&vir);
    let doubles: usize = fns
        .iter()
        .map(|(_, b)| element_accesses(b, &arrays, "f64"))
        .sum();
    assert!(doubles > 0, "no double element access\n{vir}");
    for (header, body) in &fns {
        assert_eq!(
            element_accesses(body, &arrays, "i64"),
            0,
            "{header}: an array here must keep doubles\n{}",
            body.join("\n")
        );
    }
}
