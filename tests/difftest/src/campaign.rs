//! `difftest fuzz` and `difftest shrink`: generate programs for a seed range, check them in
//! parallel, group failures by verdict signature (one group ≈ one bug), and minimize the first
//! program of each group. Everything lands in `<out>/cases/<signature>/`.

use crate::check::{self, Config, Mode, Verdict};
use crate::{gen, pool, report, shrink};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Runs a campaign over `seeds`; returns the number of distinct bug signatures.
pub fn fuzz(
    cfg: &Config,
    opts: gen::Options,
    seeds: &[u64],
    jobs: usize,
    out: &Path,
) -> Result<usize, String> {
    let work = out.join("work");
    let verdicts = pool::map(
        seeds,
        jobs,
        &work,
        |&seed, scratch| check_seed(cfg, opts, seed, scratch),
        |i, seed, v| {
            if interesting(v) {
                println!("seed {seed}: {}", v.signature());
            } else if (i + 1) % 100 == 0 {
                println!("... {} seeds checked", i + 1);
            }
        },
    );
    let mut groups: BTreeMap<String, Vec<(u64, Verdict)>> = BTreeMap::new();
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for (&seed, v) in seeds.iter().zip(verdicts) {
        let kind = v.signature().split(':').next().unwrap_or("").to_string();
        *tally.entry(kind).or_default() += 1;
        if interesting(&v) {
            groups.entry(v.signature()).or_default().push((seed, v));
        }
    }
    let firsts: Vec<(u64, Verdict)> = groups.values().map(|g| g[0].clone()).collect();
    pool::map(
        &firsts,
        jobs,
        &out.join("shrink"),
        |(seed, v), scratch| minimize_seed(cfg, opts, *seed, v, out, scratch),
        |_, (seed, _), r| match r {
            Ok(path) => println!("shrunk seed {seed} -> {path}"),
            Err(e) => println!("shrinking seed {seed} failed: {e}"),
        },
    );
    let summary: Vec<String> = tally.iter().map(|(k, n)| format!("{n} {k}")).collect();
    println!("{} seeds: {}", seeds.len(), summary.join(", "));
    for (sig, g) in &groups {
        let examples: Vec<String> = g.iter().take(5).map(|(s, _)| s.to_string()).collect();
        println!("  {:>4} × {sig}  (seeds {})", g.len(), examples.join(", "));
    }
    Ok(groups
        .keys()
        .filter(|s| s.starts_with("mismatch") || s.starts_with("crash"))
        .count())
}

/// Minimizes the program in `file`, keeping its current verdict; prints and saves the result.
pub fn shrink_file(cfg: &Config, file: &Path, out: &Path) -> Result<(), String> {
    let text =
        fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let scratch = out.join("shrink").join("w0");
    let verdict = check::check_source(cfg, &text, &scratch)?;
    if matches!(
        verdict,
        Verdict::Agree | Verdict::NodeRejected(_) | Verdict::OracleTimeout
    ) {
        return Err(format!("nothing to shrink: {}", verdict.signature()));
    }
    let min = minimize(cfg, &text, &verdict, &scratch);
    let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("prog");
    let saved = report::save(out, &format!("{name}.min"), &min, &verdict)?;
    print!("{min}");
    eprintln!("{} -> {}", verdict.signature(), saved.display());
    Ok(())
}

fn check_seed(cfg: &Config, opts: gen::Options, seed: u64, scratch: &Path) -> Verdict {
    check::check_source(cfg, &gen::program(seed, opts), scratch)
        .unwrap_or_else(Verdict::NodeRejected)
}

/// Bugs, plus programs `velt` rejects: those are generator mistakes or real front-end gaps, and
/// either way worth a look.
fn interesting(v: &Verdict) -> bool {
    v.is_bug() || matches!(v, Verdict::VeltRejected(_))
}

fn minimize_seed(
    cfg: &Config,
    opts: gen::Options,
    seed: u64,
    verdict: &Verdict,
    out: &Path,
    scratch: &Path,
) -> Result<String, String> {
    let text = gen::program(seed, opts);
    report::save(out, &format!("seed-{seed}"), &text, verdict)?;
    let min = minimize(cfg, &text, verdict, scratch);
    let path = report::save(out, &format!("seed-{seed}.min"), &min, verdict)?;
    Ok(path.display().to_string())
}

/// Shrinks `text` while it keeps `verdict`'s signature, re-checking only the failing mode.
fn minimize(cfg: &Config, text: &str, verdict: &Verdict, scratch: &Path) -> String {
    let mode = match verdict {
        Verdict::Mismatch { mode, .. } | Verdict::CompilerCrash { mode, .. } => *mode,
        _ => Mode::Debug,
    };
    let narrow = Config {
        modes: vec![mode],
        ..cfg.clone()
    };
    let want = verdict.signature();
    shrink::shrink(text, |candidate| {
        check::check_source(&narrow, candidate, scratch).is_ok_and(|v| v.signature() == want)
    })
}

/// Parses `A..B` (exclusive end) or a single seed.
pub fn parse_seeds(spec: &str) -> Result<Vec<u64>, String> {
    let bad = || format!("bad seed range `{spec}` (expected `A..B` or `N`)");
    match spec.split_once("..") {
        Some((a, b)) => {
            let (a, b): (u64, u64) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
            Ok((a..b).collect())
        }
        None => Ok(vec![spec.parse().map_err(|_| bad())?]),
    }
}
