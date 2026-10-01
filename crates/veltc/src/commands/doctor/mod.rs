//! `velt doctor`: checks that the toolchain is usable on this machine and says how to fix what is
//! not. [`checks`] inspects the environment (runtime library, std, linker, clang, vpm home);
//! [`smoke`] compiles and runs a hello-world. Clang is optional (warning), everything else required.

mod checks;
mod smoke;

use std::process::ExitCode;

/// How one check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Works.
    Ok,
    /// Optional feature unavailable; does not fail `doctor`.
    Warn,
    /// Required and broken: `doctor` exits 1.
    Fail,
}

/// One line of the report.
#[derive(Debug)]
pub struct Check {
    /// Short label (`runtime lib`, `linker`...).
    pub name: &'static str,
    /// Outcome.
    pub status: Status,
    /// What was found (path, version) or what went wrong.
    pub detail: String,
    /// How to fix a warning or failure.
    pub hint: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Check {
        Check {
            name,
            status: Status::Ok,
            detail: detail.into(),
            hint: None,
        }
    }

    fn bad(
        name: &'static str,
        status: Status,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Check {
        Check {
            name,
            status,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }
}

/// `velt doctor`: print every check; exit 1 if a required one failed.
pub fn doctor_command() -> ExitCode {
    let mut all = checks::environment();
    let clang_found = all
        .iter()
        .any(|c| c.name == checks::CLANG && c.status == Status::Ok);
    all.extend(smoke::run(clang_found));
    print!("{}", render(&all));
    if all.iter().any(|c| c.status == Status::Fail) {
        println!("\nsome required checks failed (see the `fix:` lines above)");
        ExitCode::from(1)
    } else {
        println!("\nall required checks passed");
        ExitCode::SUCCESS
    }
}

/// The report: one `✓`/`!`/`✗` line per check, with an indented fix hint under problems.
fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for c in checks {
        let mark = match c.status {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
        };
        out.push_str(&format!("{mark} {:<width$}  {}\n", c.name, c.detail));
        if let Some(hint) = &c.hint {
            for line in hint.lines() {
                out.push_str(&format!("  {:<width$}  fix: {line}\n", ""));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_marks_and_hints() {
        let text = render(&[
            Check::ok("std", "/x/std"),
            Check::bad("clang", Status::Warn, "not found", "install LLVM"),
            Check::bad("linker", Status::Fail, "missing", "install cc"),
        ]);
        assert_eq!(
            text,
            "✓ std     /x/std\n! clang   not found\n          fix: install LLVM\n✗ linker  missing\n          fix: install cc\n"
        );
    }
}
