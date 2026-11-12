//! Error-set variables change nothing but the cost (#2590, `velt_sema::throws::vars`): every
//! golden that mentions `Promise` or `async`, a sample of those that report errors or warnings
//! ([`reports`]), and every program in `site_vars_identity/`, built
//! with `--emit vir` with the variables and with `VELT_NO_SITE_VARS=1` (the sets found by checking
//! the program again), gives the same VIR and warnings, or fails with the same diagnostics, and
//! the check with the variables of a program that builds takes no more rounds than the one
//! without (one that fails at most one more: it is checked again without variables, for its
//! diagnostics). A program that fails reports no error that the check with variables did not
//! report before it was checked again (`VELT_SITE_VARS_FIRST`): that check again can be wrong
//! where a check without variables is. The programs in [`KNOWN`] are compared too, and must
//! differ as recorded: one that no longer differs leaves the list, and one that differs
//! otherwise is looked at again.
//!
//! Long, a part of `eager_std_identity`, which `cargo xtask affected` runs with `--ignored`. With
//! `VELT_VIR_IDENTITY=sample` it compares one program in [`SAMPLE_ONE_IN`].

use super::{no_window, test_dir};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Programs that differ with and without variables, or take more rounds with them, before
/// #2590 too (function types written without `throws`): #2602. Each with how it differs, which
/// the test checks: the side that builds, the first function or layout whose VIR differs, the
/// first diagnostic message that differs (nothing that depends on the OS).
const KNOWN: &[(&str, Kind)] = &[
    (
        "golden/bugs/lower_adapted_closure_kept.velt",
        Kind::Vir("callsIt state"),
    ),
    (
        "golden/lang/any_fn_members.velt",
        Kind::Vir("_V4read_T16bool_20_7c_20f64"),
    ),
    (
        "golden/lang/ctor_value_identity.velt",
        Kind::Vir(" new (f64) => A }"),
    ),
    ("golden/lang/ctor_values.velt", Kind::Vir("T[]")),
    (
        "golden/lang/generator_expression_captures.velt",
        Kind::Rounds,
    ),
    (
        "golden/lang/generator_expression_contextual_type.velt",
        Kind::Rounds,
    ),
    ("golden/lang/jsx_async_package_provider.velt", Kind::Rounds),
    (
        "golden/lang/promise_settled_borrowed_values.velt",
        Kind::Rounds,
    ),
    ("golden/lang/rethrow_never_thrown_catch.velt", Kind::Rounds),
    (
        "golden/lang/throws_fn_type_rethrow_chain.velt",
        Kind::Builds(true),
    ),
    (
        "golden/lang/throws_fn_type_values.velt",
        Kind::Vir("run::{closure#1} env"),
    ),
    (
        "golden/std/catch_panic.velt",
        Kind::Vir("Error | Refused | PanicError::Error"),
    ),
    (
        "golden/std/throws_fn_type_revivers.velt",
        Kind::Vir("RegExpError | UrlError::RegExpError"),
    ),
    // No promise: `const b: G = a` makes the sets of `F` and `G` one with variables, so the
    // `catch` around `a(1)` may catch an `E2`; without them `F`'s set is `E1` alone, and the
    // `instanceof E2` test is rejected as always false. The same before #2590: #2602.
    ("site_vars_identity/review_u1.velt", Kind::Builds(true)),
    // A store a check without variables misses (an argument after a spread), so its VIR differs,
    // before #2590 too: #2603.
    (
        "site_vars_identity/review_sp5.velt",
        Kind::Vir("main state"),
    ),
    // A check without variables rejects this program: the middleware chain built in a loop
    // stores rejecting promises it does not count (#2602). With variables it builds, and with
    // another error it reports that error alone (the check again adds none).
    ("site_vars_identity/review_pr_p06.velt", Kind::Builds(true)),
    (
        "site_vars_identity/review_pr_p06_error.velt",
        Kind::Diagnostics("error: mismatched types"),
    ),
];

/// Where the VIR starts in what [`vir`] returns for a program that builds.
const VIR_MARK: &str = "\n--- vir\n";

/// One program in this many, in the sample.
const SAMPLE_ONE_IN: u64 = 8;

/// Whether the program `name` is in the sample: chosen by a hash of its name, so a new one
/// moves no other in or out.
fn in_sample(name: &str) -> bool {
    name_hash(name).is_multiple_of(SAMPLE_ONE_IN)
}

/// A hash of `name`, the same on every OS.
fn name_hash(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// One golden that reports errors in this many, besides those with promises: a check that
/// reports anything may be checked again (`throws::vars`), also one without sites.
const ERRORS_ONE_IN: u64 = 4;

/// Does the golden `p` report something: errors (a `.err` file), or warnings (in its
/// `.stderr`)? Those with errors are taken one in [`ERRORS_ONE_IN`], by name.
fn reports(p: &Path, name: &str) -> bool {
    let warns =
        std::fs::read_to_string(p.with_extension("stderr")).is_ok_and(|s| s.contains("warning"));
    let errs = p.with_extension("err").exists() && name_hash(name) % ERRORS_ONE_IN == 1;
    warns || errs
}

/// The `.velt` files under `dir` that mention `Promise` or `async`, or report something
/// ([`reports`]) (`all`: every one).
fn programs(dir: &Path, all: bool, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            programs(&p, all, out);
        } else if name.ends_with(".velt") && !name.starts_with('_') {
            let src = std::fs::read_to_string(&p).unwrap_or_default();
            if all || src.contains("Promise") || src.contains("async") || reports(&p, &name) {
                out.push(p);
            }
        }
    }
}

/// `--emit vir` of `file`: whether it built, what it reported (without the timing lines) and the
/// VIR when it built, and how many rounds the check took.
fn vir(file: &Path, home: &Path, mode: Mode) -> (bool, String, usize) {
    let mut c = no_window::command(env!("CARGO_BIN_EXE_velt"));
    c.arg("build")
        .arg(file)
        .args(["--emit", "vir"])
        .current_dir(file.parent().unwrap())
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .env("VELT_SEMA_TIMINGS", "1");
    c.env_remove("VELT_NO_SITE_VARS")
        .env_remove("VELT_SITE_VARS_FIRST");
    match mode {
        Mode::Vars => {}
        Mode::NoVars => drop(c.env("VELT_NO_SITE_VARS", "1")),
        Mode::FirstCheck => drop(c.env("VELT_SITE_VARS_FIRST", "1")),
    }
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

/// How [`vir`] checks a program.
#[derive(Clone, Copy)]
enum Mode {
    /// With error-set variables.
    Vars,
    /// Without them (`VELT_NO_SITE_VARS`).
    NoVars,
    /// With them, its diagnostics as the first check reports them, not checked again
    /// (`VELT_SITE_VARS_FIRST`).
    FirstCheck,
}

/// How a program differs with and without variables, as [`KNOWN`] records it: nothing that
/// depends on the OS (paths, `agg#` numbers, line text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// It builds with variables only (`true`), or without them only.
    Builds(bool),
    /// Both build, to different VIR: the first difference is in this function (its symbol) or
    /// layout (its name).
    Vir(&'static str),
    /// What it reported differs (the errors of a program that fails, the warnings of one that
    /// builds), first in this message (without its place).
    Diagnostics(&'static str),
    /// It builds the same, in more rounds with variables.
    Rounds,
}

/// [`Kind`] with what it names found now.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Builds(bool),
    Vir(String),
    Diagnostics(String),
    Rounds,
    /// A program that fails reports an error the first check with variables did not: never
    /// in [`KNOWN`].
    NewError,
}

impl Found {
    fn is(&self, k: Kind) -> bool {
        match (self, k) {
            (Found::Builds(a), Kind::Builds(b)) => *a == b,
            (Found::Vir(a), Kind::Vir(b)) | (Found::Diagnostics(a), Kind::Diagnostics(b)) => a == b,
            (Found::Rounds, Kind::Rounds) => true,
            _ => false,
        }
    }
}

/// Where VIR line `i` of `lines` is: the symbol of the function it is in, else the name of the
/// layout it is, else the line without numbers.
fn vir_place(lines: &[&str], i: usize) -> String {
    let after_id = |l: &str| {
        l.split_once(' ')
            .map_or(String::new(), |(_, r)| r.to_string())
    };
    if let Some(f) = lines[..=i].iter().rev().find(|l| l.starts_with("fn#")) {
        let head = f.split('(').next().unwrap_or(f);
        return head.rsplit(' ').next().unwrap_or(head).to_string();
    }
    let l = lines[i];
    match l.starts_with("agg#") {
        true => after_id(l).split(" size=").next().unwrap_or("").to_string(),
        false => l.chars().filter(|c| !c.is_ascii_digit()).collect(),
    }
}

/// A diagnostic line without its place (`file:1:2: error: m` is `error: m`).
fn diagnostic_text(l: &str) -> String {
    let t = l.trim();
    ["error: ", "warning: "]
        .iter()
        .find_map(|k| t.find(k).map(|i| t[i..].to_string()))
        .unwrap_or_else(|| t.to_string())
}

/// The places (`file:line:col`) of the errors in what [`vir`] reported.
fn error_places(text: &str) -> Vec<&str> {
    (text.lines())
        .filter_map(|l| l.split_once(": error: ").map(|(at, _)| at))
        .collect()
}

/// How `file` differs with and without variables, if it does: what it is, and the text.
fn differs(file: &Path, home: &Path, built: &AtomicUsize) -> Option<(Found, String)> {
    let (vars_ok, with, rounds) = vir(file, home, Mode::Vars);
    let (plain_ok, without, plain_rounds) = vir(file, home, Mode::NoVars);
    if vars_ok && plain_ok {
        built.fetch_add(1, Ordering::Relaxed);
    }
    // A program that fails reports no error the check with variables did not (the check
    // again, without them, can be wrong where a check without variables is: #2602).
    if !vars_ok {
        let (_, first, _) = vir(file, home, Mode::FirstCheck);
        let before = error_places(&first);
        if let Some(at) = error_places(&with)
            .into_iter()
            .find(|at| !before.contains(at))
        {
            let why = format!("{at}: an error the check with variables did not report");
            return Some((Found::NewError, why));
        }
    }
    if vars_ok != plain_ok {
        let why = format!("builds with variables: {vars_ok}; without: {plain_ok}");
        return Some((Found::Builds(vars_ok), why));
    }
    if with != without {
        let (w, o): (Vec<&str>, Vec<&str>) = (with.lines().collect(), without.lines().collect());
        let i = (0..w.len().max(o.len()))
            .find(|&i| w.get(i) != o.get(i))
            .unwrap_or(0);
        let why = format!(
            "with variables: {}\n  without: {}",
            w.get(i).unwrap_or(&""),
            o.get(i).unwrap_or(&"")
        );
        let vir_of = |t: &str| t.split_once(VIR_MARK).map(|(_, v)| v.to_string());
        let found = match (vir_of(&with), vir_of(&without)) {
            (Some(a), Some(b)) if a != b => {
                let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
                let j = (0..a.len().min(b.len())).find(|&j| a[j] != b[j]);
                let (lines, j) = match j {
                    Some(j) => (&a, j),
                    None if a.len() > b.len() => (&a, b.len()),
                    None => (&b, a.len().min(b.len().saturating_sub(1))),
                };
                Found::Vir(vir_place(lines, j))
            }
            _ => Found::Diagnostics(diagnostic_text(w.get(i).or(o.get(i)).unwrap_or(&""))),
        };
        return Some((found, why));
    }
    // One that fails may take one more, checked again without variables for its diagnostics.
    let allowed = plain_rounds + usize::from(!vars_ok);
    (rounds > allowed).then(|| {
        let why = format!("{rounds} rounds with variables, {plain_rounds} without");
        (Found::Rounds, why)
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
                        (Some((found, why)), Some(was)) if !found.is(was) => format!(
                            "differs otherwise than `KNOWN` says ({was:?}): {found:?}\n  {why}"
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
