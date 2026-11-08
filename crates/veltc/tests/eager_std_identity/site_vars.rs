//! Error-set variables change nothing but the cost (#2590, `velt_sema::throws::vars`): every
//! golden that mentions `Promise` or `async`, and every program in `site_vars_identity/`, built
//! with `--emit vir` with the variables and with `VELT_NO_SITE_VARS=1` (the sets found by checking
//! the program again), gives the same VIR, or fails with the same diagnostics, and the check
//! with the variables takes no more rounds than the one without.
//!
//! Long, a part of `eager_std_identity`, which `cargo xtask affected` runs with `--ignored`. With
//! `VELT_VIR_IDENTITY=sample` it compares one program in [`SAMPLE_ONE_IN`].

use super::{no_window, test_dir};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Programs that differ with and without variables, or take more rounds with them, before
/// #2590 too (function types written without `throws`): #2602.
const KNOWN: &[&str] = &[
    "golden/bugs/lower_adapted_closure_kept.velt",
    "golden/lang/any_fn_members.velt",
    "golden/lang/ctor_value_identity.velt",
    "golden/lang/ctor_values.velt",
    "golden/lang/errors/assign_target_replaced_holder_values.velt",
    "golden/lang/errors/http_server/http_handler_store_by_key.velt",
    "golden/lang/generator_expression_captures.velt",
    "golden/lang/generator_expression_contextual_type.velt",
    "golden/lang/jsx_async_package_provider.velt",
    "golden/lang/overloads_more.velt",
    "golden/lang/promise_settled_borrowed_values.velt",
    "golden/lang/promises_new_resolve_fn.velt",
    "golden/lang/rethrow_never_thrown_catch.velt",
    "golden/lang/throws_fn_type_rethrow_chain.velt",
    "golden/lang/throws_fn_type_values.velt",
    "golden/std/catch_panic.velt",
    "golden/std/throws_fn_type_revivers.velt",
    "site_vars_identity/review_u1.velt",
    // A store a check without variables misses (an argument after a spread): #2603.
    "site_vars_identity/review_sp5.velt",
];

/// One program in this many, in the sample.
const SAMPLE_ONE_IN: u64 = 8;

/// Whether the program `name` is in the sample: chosen by a hash of its name, so a new one
/// moves no other in or out.
fn in_sample(name: &str) -> bool {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    hash % SAMPLE_ONE_IN == 0
}

/// The `.velt` files under `dir` that mention `Promise` or `async` (`all`: every one).
fn programs(dir: &Path, all: bool, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            programs(&p, all, out);
        } else if name.ends_with(".velt") && !name.starts_with('_') {
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            if all || src.contains("Promise") || src.contains("async") {
                out.push(p);
            }
        }
    }
}

/// `--emit vir` of `file`: whether it built, the VIR or what it reported (without the timing
/// lines), and how many rounds the check took.
fn vir(file: &Path, home: &Path, vars: bool) -> (bool, String, usize) {
    let mut c = no_window::command(env!("CARGO_BIN_EXE_velt"));
    c.arg("build")
        .arg(file)
        .args(["--emit", "vir"])
        .current_dir(file.parent().unwrap())
        .env("VELT_HOME", home)
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .env("VELT_SEMA_TIMINGS", "1");
    match vars {
        true => c.env_remove("VELT_NO_SITE_VARS"),
        false => c.env("VELT_NO_SITE_VARS", "1"),
    };
    let out = c.output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    // `velt-sema:   round: first   1.2 ms  x1`: the totals, one line per kind of round.
    let rounds = (err.lines())
        .filter(|l| l.starts_with("velt-sema:   round: "))
        .filter_map(|l| {
            l.rsplit_once(" x")
                .and_then(|(_, n)| n.trim().parse::<usize>().ok())
        })
        .sum();
    let reported: Vec<&str> = err
        .lines()
        .filter(|l| !l.starts_with("velt-sema:"))
        .collect();
    let text = match out.status.success() {
        true => String::from_utf8_lossy(&out.stdout).into_owned(),
        false => reported.join("\n"),
    };
    (out.status.success(), text, rounds)
}

/// How `file` differs with and without variables, if it does.
fn differs(file: &Path, home: &Path, built: &AtomicUsize) -> Option<String> {
    let (vars_ok, with, rounds) = vir(file, home, true);
    let (plain_ok, without, plain_rounds) = vir(file, home, false);
    if vars_ok && plain_ok {
        built.fetch_add(1, Ordering::Relaxed);
    }
    if vars_ok != plain_ok {
        return Some(format!(
            "builds with variables: {vars_ok}; without: {plain_ok}"
        ));
    }
    if with != without {
        let found = with.lines().zip(without.lines()).find(|(x, y)| x != y);
        return Some(match found {
            Some((x, y)) => format!("with variables: {x}\n  without: {y}"),
            None => "one is longer".into(),
        });
    }
    (rounds > plain_rounds)
        .then(|| format!("{rounds} rounds with variables, {plain_rounds} without"))
}

#[test]
#[ignore = "builds every golden with `Promise` twice: `cargo xtask check` runs it with the identity above"]
fn programs_with_promises_are_the_same_with_and_without_error_set_variables() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let root = tests.join("../../../tests");
    let mut files = vec![];
    programs(&root.join("golden"), false, &mut files);
    programs(&tests.join("site_vars_identity"), true, &mut files);
    files.sort();
    let name_of = |p: &Path| -> String {
        let name = match p.strip_prefix(&root) {
            Ok(n) => n.to_path_buf(),
            Err(_) => p.strip_prefix(&tests).unwrap_or(p).to_path_buf(),
        };
        name.to_string_lossy().replace('\\', "/")
    };
    assert!(files.len() > 900, "{} programs", files.len());
    if let Some(f) = std::env::var_os("VELT_GOLDEN") {
        let f = f.to_string_lossy().into_owned();
        files.retain(|p| p.to_string_lossy().contains(&f));
    }
    let filtered = std::env::var_os("VELT_GOLDEN").is_some();
    let sample = std::env::var("VELT_VIR_IDENTITY").is_ok_and(|v| v == "sample");
    if sample {
        files.retain(|p| in_sample(&name_of(p)));
        eprintln!("the sample: {} programs", files.len());
    }
    files.retain(|p| !KNOWN.contains(&name_of(p).as_str()));
    let next = AtomicUsize::new(0);
    let built = AtomicUsize::new(0);
    let found = Mutex::new(vec![]);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
    let home = test_dir::TestDir::new();
    std::thread::scope(|s| {
        for w in 0..workers {
            let (next, built, found, files) = (&next, &built, &found, &files);
            let home = home.path().join(w.to_string());
            let name_of = &name_of;
            s.spawn(move || {
                while let Some(file) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(why) = differs(file, &home, built) {
                        found.lock().unwrap().push((name_of(file), why));
                    }
                }
            });
        }
    });
    let built = built.into_inner();
    assert!(
        filtered || built > if sample { 80 } else { 700 },
        "only {built} of {} programs built",
        files.len()
    );
    let mut found = found.into_inner().unwrap();
    found.sort();
    let listed: Vec<String> = found
        .iter()
        .map(|(n, why)| format!("{n}\n  {why}"))
        .collect();
    assert!(
        listed.is_empty(),
        "{} programs differ with and without error-set variables:\n{}",
        listed.len(),
        listed.join("\n")
    );
}
