//! A SIMD helper gets a counted array's header where it is, not a copy (velt_vir
//! `lower/call.rs`): after inlining, LLVM would redo a copy after every element store, since a
//! copy is no access the array alias tags describe (velt_codegen_llvm `tbaa.rs`).

use std::path::Path;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

#[test]
fn simd_loads_read_the_header_in_place() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/simd_in_place/main.vlt");
    let opts = BuildOptions {
        input,
        release: false,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    let vir = match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    };
    let array = vir
        .lines()
        .find_map(|l| l.strip_prefix("agg#").filter(|l| l.contains(" T[] ")))
        .and_then(|l| l.split(' ').next())
        .unwrap_or_else(|| panic!("no array layout in\n{vir}"));
    // A whole header read into a local: `_n = (*_m as agg#A)`, nothing projected after it.
    let copy = format!(" as agg#{array})");
    let mut loads = 0;
    for function in vir.split("\nfn#").skip(1) {
        if !function.contains(" velt_simd_u8x16_load(") {
            continue;
        }
        loads += 1;
        let copies: Vec<&str> = function
            .lines()
            .filter(|l| l.trim_start().starts_with('_') && l.ends_with(&copy))
            .collect();
        assert!(
            copies.is_empty(),
            "header copies {copies:?} in\nfn#{function}"
        );
    }
    assert!(loads > 0, "no SIMD load in\n{vir}");
}
