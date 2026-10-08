//! `tests/golden/lang/opt_heap_objects_aliases.vlt` checks that every name of an object sees
//! every write; this checks that its objects really leave the heap in a release build, so the
//! golden exercises `heap_sroa`'s copies between names (#558) rather than heap objects.

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

/// The number of allocations in each function of `vir` that allocates, by function header.
fn allocations(vir: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut current = String::new();
    for line in vir.lines() {
        if line.starts_with("fn#") {
            current = line.to_string();
        } else if line.contains("= call extern#") && line.contains(" velt_rt_alloc(") {
            match out.last_mut() {
                Some((f, n)) if *f == current => *n += 1,
                _ => out.push((current.clone(), 1)),
            }
        }
    }
    out
}

#[test]
fn objects_under_several_names_leave_the_heap() {
    let vir = release_vir("lang/opt_heap_objects_aliases.vlt");
    // `built`, `accumulated` and `renamed` (inlined into `main` or not) allocate nothing: their
    // objects are written through one name and read through another. Only `partial`'s two
    // objects stay, because `q` holds `a` on one path only.
    let allocs = allocations(&vir);
    let partial = allocs.iter().filter(|(f, _)| f.contains("partial"));
    assert_eq!(
        partial.map(|(_, n)| n).sum::<usize>(),
        2,
        "{allocs:?}\n{vir}"
    );
    assert_eq!(allocs.len(), 1, "{allocs:?}\n{vir}");
}
