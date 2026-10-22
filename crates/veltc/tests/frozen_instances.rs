//! Frozen instances (velt_vir `lower/frozen.rs`, #708) read values other threads read at the same
//! time, whose reference counts are not atomic: they must not change a count or share a value.
//! This lowers the programs that read `shared<T>` values and checks every `Work::Frozen`
//! instance (symbol suffix `_Z<mask>`) in their VIR: no access to an object's count word
//! (`ptradd …, -8_i64`, velt_vir `rc.rs`) and no call of share glue (`_Gshare_…`,
//! `_Gdynshare_…`). Copies (clone glue) are what such an instance takes instead.

use std::path::Path;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The programs checked, relative to the repository root.
const PROGRAMS: &[&str] = &[
    "tests/golden/lang/shared_frozen_calls.vlt",
    "tests/golden/lang/shared_reads.vlt",
    "tests/golden/lang/shared_reads_counted.vlt",
    "tests/golden/lang/shared_reads_replaced.vlt",
    "tests/golden/lang/shared_reads_through.vlt",
    "tests/golden/lang/shared_frozen_loops.vlt",
    "bench/shared/store_reads.vlt",
];

fn vir(path: &str, release: bool) -> String {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    let opts = BuildOptions {
        input,
        release,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{path}: {e:?}\n{}", sess.render_diagnostics()),
    }
}

/// Is the function header `line` (`fn#N internal <symbol>(…`) a frozen instance's?
fn frozen_instance(line: &str) -> bool {
    let Some(sym) = line.split_whitespace().nth(2) else {
        return false;
    };
    let sym = sym.split('(').next().unwrap_or(sym);
    sym.match_indices("_Z").any(|(i, _)| {
        sym[i + 2..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_hexdigit())
    })
}

/// The lines of frozen instances in `vir` that count or share, with their function's header.
fn violations(vir: &str) -> (usize, Vec<String>) {
    let (mut instances, mut out) = (0, vec![]);
    let mut current: Option<&str> = None;
    for line in vir.lines() {
        if line.starts_with("fn#") {
            current = frozen_instance(line).then_some(line);
            instances += usize::from(current.is_some());
            continue;
        }
        let Some(f) = current else {
            continue;
        };
        let counts = line.contains("ptradd") && line.contains(", -8_i64");
        let shares = line.contains("_Gshare_") || line.contains("_Gdynshare_");
        if counts || shares {
            out.push(format!("{f}\n    {}", line.trim()));
        }
    }
    (instances, out)
}

#[test]
fn frozen_instances_neither_count_nor_share() {
    let mut total = 0;
    for p in PROGRAMS {
        for release in [false, true] {
            let (n, bad) = violations(&vir(p, release));
            total += n;
            assert!(
                bad.is_empty(),
                "{p} (release: {release}): a frozen instance counts or shares:\n{}",
                bad.join("\n")
            );
        }
    }
    // The programs do reach frozen instances (else this checks nothing).
    assert!(total > 0, "no frozen instance in {PROGRAMS:?}");
}
