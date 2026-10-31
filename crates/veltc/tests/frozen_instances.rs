//! Frozen instances (velt_vir `lower/frozen.rs`, #708) read values other threads read at the same
//! time, whose reference counts are not atomic: they must not change a count or share a value.
//! This lowers the programs that read `shared<T>` values and checks every `Work::Frozen`
//! instance (symbol suffix `_Z<mask>`) in their VIR: no access to the count word of an object
//! it did not allocate itself (`ptradd …, -8_i64`, velt_vir `rc.rs`; a local array the instance
//! builds, as std's JSON string writer does when it escapes, is its own) and no call of share
//! glue (`_Gshare_…`, `_Gdynshare_…`). Copies (clone glue) are what such an instance takes
//! instead.

use std::collections::{HashMap, HashSet};
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
    let mut functions: Vec<(&str, Vec<&str>)> = vec![];
    for line in vir.lines() {
        if line.starts_with("fn#") {
            functions.push((line, vec![]));
        } else if let Some((_, body)) = functions.last_mut() {
            body.push(line.trim());
        }
    }
    for (f, body) in functions.iter().filter(|(f, _)| frozen_instance(f)) {
        instances += 1;
        let own = own_objects(body);
        for line in body {
            let counted = (line.split_once("ptradd "))
                .and_then(|(_, rest)| rest.strip_suffix(", -8_i64"))
                .is_some_and(|p| !own.contains(p));
            let shares = line.contains("_Gshare_") || line.contains("_Gdynshare_");
            if counted || shares {
                out.push(format!("{f}\n    {line}"));
            }
        }
    }
    (instances, out)
}

/// The locals of a function `body` that only ever hold an object the function allocated
/// (`_b = ptradd _a, 8_i64` after `_a = call … velt_rt_alloc(…)`, copies of such a local) or
/// null: its count is the function's own.
fn own_objects<'a>(body: &[&'a str]) -> HashSet<&'a str> {
    let mut sources: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut allocated: HashSet<&str> = HashSet::new();
    for line in body {
        let Some((dst, rhs)) = line.split_once(" = ") else {
            continue;
        };
        if !dst.starts_with('_') || dst.contains(' ') {
            continue;
        }
        if rhs.contains("velt_rt_alloc(") {
            allocated.insert(dst);
        }
        sources.entry(dst).or_default().push(rhs);
    }
    let mut own: HashSet<&str> = HashSet::new();
    loop {
        let before = own.len();
        for (dst, rhs) in &sources {
            let all_own = rhs.iter().all(|r| {
                *r == "0_ptr"
                    || own.contains(r)
                    || (r.strip_prefix("ptradd "))
                        .and_then(|r| r.strip_suffix(", 8_i64"))
                        .is_some_and(|a| allocated.contains(a))
            });
            if all_own {
                own.insert(dst);
            }
        }
        if own.len() == before {
            return own;
        }
    }
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

/// A frozen instance may drop an object it allocated (its count is its own), not count one it
/// was given.
#[test]
fn own_objects_are_not_violations() {
    let vir = "\
fn#1 internal f_Z1(ptr) -> unit {
  bb0:
    _2 = 0_ptr
    _3 = call extern#0 velt_rt_alloc(32_u64, 8_u64) -> bb1
  bb1:
    _4 = ptradd _3, 8_i64
    _2 = _4
    _5 = _2
    _6 = ptradd _5, -8_i64
    _7 = ptradd _0, -8_i64
    return
}
";
    let (n, bad) = violations(vir);
    assert_eq!(n, 1);
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("ptradd _0, -8_i64"), "{bad:?}");
}
