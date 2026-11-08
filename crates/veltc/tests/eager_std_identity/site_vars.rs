//! Error-set variables change nothing but the cost (#2590, `velt_sema::throws::vars`): every
//! golden that mentions `Promise` or `async`, and every program in `site_vars_identity/`, built
//! with `--emit vir` with the variables and with `VELT_NO_SITE_VARS=1` (the sets found by checking
//! the program again), gives the same VIR and warnings, or fails with the same diagnostics, and
//! the check with the variables of a program that builds takes no more rounds than the one
//! without (one that fails is checked again without variables, for its diagnostics). The
//! programs in [`KNOWN`] are compared too, and must differ as recorded: one that no longer
//! differs leaves the list, and one that differs otherwise is looked at again.
//!
//! Long, a part of `eager_std_identity`, which `cargo xtask affected` runs with `--ignored`. With
//! `VELT_VIR_IDENTITY=sample` it compares one program in [`SAMPLE_ONE_IN`].

use super::{no_window, test_dir};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Programs that differ with and without variables, or take more rounds with them, before
/// #2590 too (function types written without `throws`): #2602. Each with the kind of
/// difference, which the test checks (the text of one depends on the OS).
const KNOWN: &[(&str, Kind)] = &[
    ("golden/bugs/lower_adapted_closure_kept.velt", Kind::Vir),
    ("golden/lang/any_fn_members.velt", Kind::Vir),
    ("golden/lang/ctor_value_identity.velt", Kind::Vir),
    ("golden/lang/ctor_values.velt", Kind::Vir),
    (
        "golden/lang/generator_expression_captures.velt",
        Kind::Rounds,
    ),
    (
        "golden/lang/generator_expression_contextual_type.velt",
        Kind::Rounds,
    ),
    ("golden/lang/jsx_async_package_provider.velt", Kind::Rounds),
    ("golden/lang/overloads_more.velt", Kind::Vir),
    (
        "golden/lang/promise_settled_borrowed_values.velt",
        Kind::Rounds,
    ),
    ("golden/lang/rethrow_never_thrown_catch.velt", Kind::Rounds),
    (
        "golden/lang/throws_fn_type_rethrow_chain.velt",
        Kind::Builds,
    ),
    ("golden/lang/throws_fn_type_values.velt", Kind::Vir),
    ("golden/std/catch_panic.velt", Kind::Vir),
    ("golden/std/throws_fn_type_revivers.velt", Kind::Vir),
    // No promise: `const b: G = a` makes the sets of `F` and `G` one with variables, so the
    // `catch` around `a(1)` may catch an `E2`; without them `F`'s set is `E1` alone, and the
    // `instanceof E2` test is rejected as always false. The same before #2590: #2602.
    ("site_vars_identity/review_u1.velt", Kind::Builds),
    // A store a check without variables misses (an argument after a spread), so its VIR differs,
    // before #2590 too: #2603.
    ("site_vars_identity/review_sp5.velt", Kind::Vir),
];

/// Where the VIR starts in what [`vir`] returns for a program that builds.
const VIR_MARK: &str = "\n--- vir\n";

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

/// `--emit vir` of `file`: whether it built, what it reported (without the timing lines) and the
/// VIR when it built, and how many rounds the check took.
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
    // Diagnostics name the file by its path: by its name alone, the same on every machine.
    let dir = file.parent().unwrap().to_string_lossy().into_owned();
    let local = |s: &[u8]| {
        let s = String::from_utf8_lossy(s).into_owned();
        s.replace(&format!("{dir}/"), "")
            .replace(&format!("{dir}\\"), "")
    };
    let err = local(&out.stderr);
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
    // What it reported: the diagnostics of a failed build, the warnings of one that built.
    let mut text = reported.join("\n");
    if out.status.success() {
        text.push_str(VIR_MARK);
        text.push_str(&local(&out.stdout));
    }
    (out.status.success(), text, rounds)
}

/// How a program differs with and without variables: what [`KNOWN`] records, the same on every
/// OS (the text of a difference, VIR above all, is not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// It builds with them and not without, or the other way.
    Builds,
    /// Both build, to different VIR.
    Vir,
    /// What it reported differs (the errors of a program that fails, the warnings of one that
    /// builds).
    Diagnostics,
    /// It builds the same, in more rounds with variables.
    Rounds,
}

/// How `file` differs with and without variables, if it does: its kind and what it is.
fn differs(file: &Path, home: &Path, built: &AtomicUsize) -> Option<(Kind, String)> {
    let (vars_ok, with, rounds) = vir(file, home, true);
    let (plain_ok, without, plain_rounds) = vir(file, home, false);
    if vars_ok && plain_ok {
        built.fetch_add(1, Ordering::Relaxed);
    }
    if vars_ok != plain_ok {
        let why = format!("builds with variables: {vars_ok}; without: {plain_ok}");
        return Some((Kind::Builds, why));
    }
    if with != without {
        let found = with.lines().zip(without.lines()).find(|(x, y)| x != y);
        let why = match found {
            Some((x, y)) => format!("with variables: {x}\n  without: {y}"),
            None => "one is longer".into(),
        };
        let vir_of = |t: &str| t.split_once(VIR_MARK).map(|(_, v)| v.to_string());
        let kind = match vir_of(&with) != vir_of(&without) {
            true => Kind::Vir,
            false => Kind::Diagnostics,
        };
        return Some((kind, why));
    }
    (vars_ok && rounds > plain_rounds).then(|| {
        let why = format!("{rounds} rounds with variables, {plain_rounds} without");
        (Kind::Rounds, why)
    })
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
                    let name = name_of(file);
                    let known = KNOWN.iter().find(|(n, _)| *n == name).map(|(_, k)| *k);
                    let why = match (differs(file, &home, built), known) {
                        (Some((_, why)), None) => why,
                        (None, Some(_)) => {
                            "is the same with and without variables now: remove it from `KNOWN`"
                                .into()
                        }
                        (Some((kind, why)), Some(was)) if kind != was => format!(
                            "differs otherwise than `KNOWN` says ({was:?}): {kind:?}\n  {why}"
                        ),
                        _ => continue,
                    };
                    found.lock().unwrap().push((name, why));
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
