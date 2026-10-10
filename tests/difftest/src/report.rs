//! Writes failing cases to the output directory, grouped by verdict signature so each directory is
//! one presumed bug: `<out>/cases/<slug>/<name>.vlt` plus `<name>.txt` with the details.

use crate::check::Verdict;
use std::fs;
use std::path::Path;

/// Saves the program at `src` and a description of `verdict`; returns the saved program's path.
pub fn write_case(out: &Path, src: &Path, verdict: &Verdict) -> Result<std::path::PathBuf, String> {
    let text =
        fs::read_to_string(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("prog");
    save(out, name, &text, verdict)
}

/// Saves in-memory `text` under `name` (generated and shrunk programs).
pub fn save(
    out: &Path,
    name: &str,
    text: &str,
    verdict: &Verdict,
) -> Result<std::path::PathBuf, String> {
    let dir = out.join("cases").join(slug(&verdict.signature()));
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let prog = dir.join(format!("{name}.vlt"));
    fs::write(&prog, text).map_err(|e| format!("cannot write {}: {e}", prog.display()))?;
    fs::write(dir.join(format!("{name}.txt")), describe(verdict))
        .map_err(|e| format!("cannot write details: {e}"))?;
    Ok(prog)
}

/// Human-readable details: the signature plus expected/actual output for mismatches.
pub fn describe(verdict: &Verdict) -> String {
    let mut s = format!("{}\n", verdict.signature());
    match verdict {
        Verdict::Mismatch {
            expected,
            actual,
            stderr,
            ..
        } => {
            s.push_str(&format!(
                "--- expected ({:?})\n{}",
                expected.status, expected.stdout
            ));
            s.push_str(&format!(
                "--- actual ({:?})\n{}",
                actual.status, actual.stdout
            ));
            if !stderr.is_empty() {
                s.push_str(&format!("--- velt stderr\n{stderr}"));
            }
        }
        Verdict::CompilerCrash { message, .. } => s.push_str(&format!("{message}\n")),
        Verdict::VeltRejected { diagnostic, .. } => s.push_str(&format!("{diagnostic}\n")),
        Verdict::NodeRejected(msg) => s.push_str(&format!("{msg}\n")),
        Verdict::Agree | Verdict::OracleTimeout => {}
    }
    s
}

/// A filesystem-safe, bounded directory name for a signature.
fn slug(signature: &str) -> String {
    let mut s: String = signature
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.trim_matches('-').chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs_are_path_safe() {
        assert_eq!(
            super::slug("mismatch[debug]: exit Exit(1) vs Signal"),
            "mismatch-debug-exit-exit-1-vs-signal"
        );
    }
}
