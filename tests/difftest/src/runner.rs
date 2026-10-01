//! `difftest run <paths>`: checks every `.vlt` file under the given paths and prints one line per
//! program plus a summary; details of each bug go to the report directory.

use crate::check::{self, Config, Verdict};
use crate::pool;
use crate::report;
use std::fs;
use std::path::{Path, PathBuf};

/// Checks all programs under `paths`; returns the number of bug verdicts.
pub fn run(cfg: &Config, paths: &[PathBuf], jobs: usize, out: &Path) -> Result<usize, String> {
    let mut files = Vec::new();
    for p in paths {
        collect(p, &mut files)?;
    }
    files.sort();
    if files.is_empty() {
        return Err("no .vlt files found under the given paths".into());
    }
    // Stale cases from an earlier run would read as new findings.
    let _ = fs::remove_dir_all(out.join("cases"));
    let work = out.join("work");
    let verdicts = pool::map(
        &files,
        jobs,
        &work,
        |f, scratch| check::check(cfg, f, scratch).unwrap_or_else(Verdict::NodeRejected),
        |_, f, v| println!("{:<60} {}", f.display(), short(v)),
    );
    let mut bugs = 0;
    let mut tally = std::collections::BTreeMap::<&str, usize>::new();
    for (f, v) in files.iter().zip(&verdicts) {
        *tally.entry(kind(v)).or_default() += 1;
        if v.is_bug() || matches!(v, Verdict::VeltRejected(_)) {
            report::write_case(out, f, v)?;
        }
        bugs += usize::from(v.is_bug());
    }
    let summary: Vec<String> = tally.iter().map(|(k, n)| format!("{n} {k}")).collect();
    println!("{} programs: {}", files.len(), summary.join(", "));
    Ok(bugs)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    let entries = fs::read_dir(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    for entry in entries.flatten() {
        let p = entry.path();
        let helper = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('_'));
        if p.is_dir() {
            collect(&p, out)?;
        } else if p.extension().is_some_and(|e| e == "vlt") && !helper {
            out.push(p);
        }
    }
    Ok(())
}

fn kind(v: &Verdict) -> &'static str {
    match v {
        Verdict::Agree => "agree",
        Verdict::NodeRejected(_) => "node-rejected",
        Verdict::OracleTimeout => "oracle-timeout",
        Verdict::VeltRejected(_) => "velt-rejected",
        Verdict::CompilerCrash { .. } => "CRASH",
        Verdict::Mismatch { .. } => "MISMATCH",
    }
}

fn short(v: &Verdict) -> String {
    match v {
        Verdict::Agree => "ok".into(),
        Verdict::NodeRejected(m) => format!("node-rejected: {m}"),
        _ => v.signature(),
    }
}
