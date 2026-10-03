//! The `tsc` oracle (issue #13): holds the lint's claims against a pinned TypeScript
//! (tests/tscompat-oracle, `npm ci` there) under the baseline `tsconfig` of
//! docs/internals/design/tsx.md "The common subset".
//!
//! - Every rule has a [`Claim`]: `tsc` rejects what it reports (a sample in `rejected/`), `tsc`
//!   accepts it but JavaScript runs it differently (a sample in `behaviour/`), or the claim is
//!   one `tsc` can't decide (with the reason). A rule without one fails here, without Node.
//! - Every rule fixture (`tests/cases`, its `.fixed` snapshot and `clean.ts`) goes through
//!   `tsc` a declaration at a time: one the lint passes must compile, and one it reports with a
//!   rule `tsc` rejects must not.
//! - Each rejected sample reports only its rule, and `tsc` has an error on every line the lint
//!   reports. Each behaviour sample compiles; one that compiles because `tsc` ignores the
//!   construct ([`IGNORED`]) gets the error it names once the lint's fixes are applied.
//! - A directory in `tests/cases` is a JSX provider the cases name (`./_jsx_pragma`); the
//!   project gets a TypeScript stand-in for it that re-exports the oracle's provider.
//!
//! Without Node or `tests/tscompat-oracle/node_modules` the `tsc` part is skipped with a
//! message (the pull request gate has no Node packages); `VELT_TSC_ORACLE=1` (the nightly run)
//! makes that a failure. Whether behaviour samples also differ under Node, and their fixes
//! don't, is for `tests/difftest` once the typed rules have samples.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::{apply_fixes, cases_dir, line_of, lint_module, lint_source, parse};
use velt_tscompat::{Finding, RULES};

/// What the oracle proves about a rule.
enum Claim {
    /// `tsc` rejects the construct: `rejected/<code>.ts` (or `.tsx`).
    Rejected,
    /// `tsc` accepts it but JavaScript runs it differently: `behaviour/<code>.ts`.
    Behaviour,
    /// `tsc` alone can't decide it, for the reason given.
    Unprovable(&'static str),
}

const CLAIMS: &[(&str, Claim)] = &[
    ("velt-number-type", Claim::Rejected),
    ("bool-type", Claim::Rejected),
    ("number-suffix", Claim::Rejected),
    ("int-cast", Claim::Rejected),
    ("struct", Claim::Rejected),
    ("extend", Claim::Rejected),
    ("throws", Claim::Rejected),
    ("promise-error-type", Claim::Rejected),
    ("interface-body", Claim::Rejected),
    ("velt-import", Claim::Rejected),
    (
        "outside-import",
        Claim::Unprovable(
            "`tsc` follows a relative import wherever it leads; the rule keeps the shared files \
             a closed set, which is about the client's `include`, not about the code",
        ),
    ),
    (
        "jsx-provider",
        Claim::Unprovable(
            "`tsc` compiles JSX against the provider the client's tsconfig names; `velt:jsx`, \
             the default when a file names none, has no JavaScript runtime, so the two sides \
             would render with different providers",
        ),
    ),
    ("declare-fn", Claim::Behaviour),
    ("jsx-pragma-comment", Claim::Behaviour),
];

/// Behaviour samples `tsc` accepts because it ignores the construct: `(code, TS error, why)`.
/// The sample with the lint's fixes applied is checked too: there `tsc` reads the construct and
/// reports the error, so the sample compiled only because `tsc` ignored it.
const IGNORED: &[(&str, &str, &str)] = &[(
    "jsx-pragma-comment",
    "2875",
    "the sample's line-comment pragma names a provider that doesn't exist; as a block comment, \
     `tsc` reads it and can't find the runtime",
)];

/// Lines where `tsc` rejects code the lint passes, each a rule still to write: `(file in the
/// project, line, why)`. The oracle fails when one of them compiles, so the entry goes when the
/// rule comes.
const KNOWN_GAPS: &[(&str, usize, &str)] = &[(
    "defaults.fixed.ts",
    8,
    "a destructuring default replaces `null` in Velt but only `undefined` in JavaScript, so \
     `tsc` keeps `x: number | null` (and Node computes with `null`); a typed rule (#13 step 2)",
)];

fn claim(code: &str) -> &'static Claim {
    &CLAIMS
        .iter()
        .find(|(c, _)| *c == code)
        .unwrap_or_else(|| panic!("`{code}` has no claim in CLAIMS"))
        .1
}

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/tscompat-oracle")
}

/// The samples in `tests/tscompat-oracle/<kind>`: `(code, file)`, sorted.
fn samples(kind: &str) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(oracle_dir().join(kind))
        .unwrap_or_else(|e| panic!("tests/tscompat-oracle/{kind}: {e}"))
        .map(|e| e.expect("entry").path())
        .map(|p| {
            let code = p.file_stem().unwrap().to_string_lossy().into_owned();
            (code, p)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn every_rule_has_a_claim_and_its_sample() {
    let rules: BTreeSet<&str> = RULES.iter().copied().collect();
    let claimed: BTreeSet<&str> = CLAIMS.iter().map(|(c, _)| *c).collect();
    assert_eq!(
        rules, claimed,
        "every rule in RULES needs a claim in CLAIMS"
    );
    for (kind, wanted) in [("rejected", true), ("behaviour", false)] {
        let codes: BTreeSet<&str> = CLAIMS
            .iter()
            .filter(|(_, c)| match c {
                Claim::Rejected => wanted,
                Claim::Behaviour => !wanted,
                Claim::Unprovable(_) => false,
            })
            .map(|(c, _)| *c)
            .collect();
        let have = samples(kind);
        let have: BTreeSet<&str> = have.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            codes, have,
            "tests/tscompat-oracle/{kind}: one sample per rule"
        );
    }
    for (code, _, _) in IGNORED {
        assert!(
            matches!(claim(code), Claim::Behaviour),
            "`{code}` in IGNORED is a behaviour claim"
        );
    }
}

/// A sample shows its rule and nothing else, so `tsc`'s verdict on it is about that rule.
#[test]
fn every_sample_reports_only_its_rule() {
    for (code, path) in samples("rejected").into_iter().chain(samples("behaviour")) {
        let src = std::fs::read_to_string(&path).expect("read sample");
        let codes: Vec<&str> = lint_source(&path, &src).iter().map(|f| f.code).collect();
        assert!(!codes.is_empty(), "{}: no findings", path.display());
        assert!(
            codes.iter().all(|c| *c == code),
            "{}: {codes:?}",
            path.display()
        );
    }
}

/// One diagnostic of `diagnostics.mjs`.
struct Diag {
    file: String,
    line: usize,
    code: String,
    message: String,
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (file, line, code, message) = (&self.file, self.line, &self.code, &self.message);
        write!(f, "{file}:{line}: TS{code} {message}")
    }
}

/// A file the oracle checks: its name in the project, its source, and the path it is linted as.
struct Checked {
    name: String,
    src: String,
    lint_as: PathBuf,
}

#[test]
fn tsc_agrees_with_the_lint() {
    let oracle = oracle_dir();
    let required = std::env::var_os("VELT_TSC_ORACLE").is_some_and(|v| v == "1");
    if let Err(why) = node_ready(&oracle) {
        assert!(!required, "VELT_TSC_ORACLE=1 but {why}");
        eprintln!("skipped: {why} (tests/tscompat-oracle: `npm ci`)");
        return;
    }
    let project = std::env::temp_dir().join(format!("velt-tsc-oracle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&project);
    let (cases, rejected, behaviour) = write_project(&oracle, &project);
    let ignored = write_ignored_fixes(&project, &behaviour);
    let diags = run_tsc(&oracle, &project);
    // The project's own diagnostics, and the stand-in JSX provider's.
    let checked: BTreeSet<&str> = cases
        .iter()
        .chain(&rejected)
        .chain(&behaviour)
        .chain(ignored.iter().map(|(file, _)| file))
        .map(|c| c.name.as_str())
        .collect();
    let mut failures: Vec<String> = diags
        .iter()
        .filter(|d| !checked.contains(d.file.as_str()))
        .map(|d| format!("the project: {d}"))
        .collect();
    let of = |name: &str| diags.iter().filter(|d| d.file == name).collect::<Vec<_>>();
    for file in &cases {
        check_case(file, &of(&file.name), &mut failures);
    }
    for file in &rejected {
        check_rejected(file, &of(&file.name), &mut failures);
    }
    for file in &behaviour {
        let found = of(&file.name);
        eprintln!("{}: tsc accepts it: {}", file.name, found.is_empty());
        failures.extend(
            found
                .iter()
                .map(|d| format!("tsc rejects a behaviour sample: {d}")),
        );
    }
    for (file, (_, wanted, why)) in &ignored {
        check_ignored(file, wanted, why, &of(&file.name), &mut failures);
    }
    for (code, claim) in CLAIMS {
        if let Claim::Unprovable(why) = claim {
            eprintln!("{code}: not decided by tsc: {why}");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let _ = std::fs::remove_dir_all(&project);
}

/// `Err(why)` when Node or the pinned `typescript` is missing.
fn node_ready(oracle: &Path) -> Result<(), String> {
    match command("node").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => return Err("`node` is not on PATH".into()),
    }
    if !oracle
        .join("node_modules/typescript/package.json")
        .is_file()
    {
        return Err("tests/tscompat-oracle/node_modules has no `typescript`".into());
    }
    Ok(())
}

/// Writes the fixtures and samples into `project` with a tsconfig extending the baseline;
/// returns the cases, rejected samples and behaviour samples as written.
fn write_project(oracle: &Path, project: &Path) -> (Vec<Checked>, Vec<Checked>, Vec<Checked>) {
    std::fs::create_dir_all(project).expect("create the project directory");
    let mut cases = vec![];
    let mut entries: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .expect("tests/cases")
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for path in &entries {
        if path.is_dir() {
            provider_stand_in(&project.join(path.file_name().unwrap()));
            continue;
        }
        let (stem, ext) = (stem(path), path.extension().unwrap().to_string_lossy());
        let (name, lint_as) = match ext.as_ref() {
            "vlt" => (format!("{stem}.ts"), path.clone()),
            "ts" | "tsx" => (format!("{stem}.{ext}"), path.clone()),
            // A snapshot is linted as its case, which says how to parse it.
            "fixed" => {
                let case = ["vlt", "ts", "tsx"]
                    .iter()
                    .map(|e| path.with_extension(e))
                    .find(|p| p.is_file())
                    .expect("a .fixed file has a case");
                let ext = if case.extension().unwrap() == "tsx" {
                    "tsx"
                } else {
                    "ts"
                };
                (format!("{stem}.fixed.{ext}"), case)
            }
            _ => continue,
        };
        let src = std::fs::read_to_string(path).expect("read case");
        std::fs::write(project.join(&name), &src).expect("write case");
        cases.push(Checked { name, src, lint_as });
    }
    let copy_samples = |kind: &str| -> Vec<Checked> {
        std::fs::create_dir_all(project.join(kind)).expect("create a sample directory");
        samples(kind)
            .into_iter()
            .map(|(_, path)| {
                let name = format!("{kind}/{}", path.file_name().unwrap().to_string_lossy());
                let src = std::fs::read_to_string(&path).expect("read sample");
                std::fs::write(project.join(&name), &src).expect("write sample");
                Checked {
                    name,
                    src,
                    lint_as: path,
                }
            })
            .collect()
    };
    let (rejected, behaviour) = (copy_samples("rejected"), copy_samples("behaviour"));
    let slashes = |p: PathBuf| p.to_string_lossy().replace('\\', "/");
    let base = slashes(oracle.join("tsconfig.base.json"));
    let jsx = slashes(oracle.join("jsx/jsx-runtime.d.ts"));
    let tsconfig = format!(
        "{{\n  \"extends\": \"{base}\",\n  \"files\": [\"{jsx}\"],\n  \
         \"include\": [\"**/*.ts\", \"**/*.tsx\"]\n}}\n"
    );
    std::fs::write(project.join("tsconfig.json"), tsconfig).expect("write tsconfig.json");
    (cases, rejected, behaviour)
}

/// A TypeScript stand-in for a JSX provider directory of the cases: the oracle's provider.
fn provider_stand_in(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create a provider directory");
    let reexport = "export * from \"oracle-jsx/jsx-runtime\";\n";
    std::fs::write(dir.join("jsx-runtime.d.ts"), reexport).expect("write a provider stand-in");
}

/// Writes each [`IGNORED`] sample with its fixes applied (`behaviour/<code>.fixed.<ext>`);
/// returns them with their entries.
fn write_ignored_fixes(
    project: &Path,
    behaviour: &[Checked],
) -> Vec<(Checked, &'static (&'static str, &'static str, &'static str))> {
    let mut out = vec![];
    for entry in IGNORED {
        let sample = behaviour
            .iter()
            .find(|c| stem(&c.lint_as) == entry.0)
            .unwrap_or_else(|| panic!("`{}` in IGNORED has no behaviour sample", entry.0));
        let src = apply_fixes(&sample.src, &lint_source(&sample.lint_as, &sample.src));
        assert_ne!(src, sample.src, "{}: the lint has no fix", sample.name);
        let ext = sample.lint_as.extension().unwrap().to_string_lossy();
        let name = format!("behaviour/{}.fixed.{ext}", entry.0);
        std::fs::write(project.join(&name), &src).expect("write a fixed sample");
        let lint_as = sample.lint_as.clone();
        out.push((Checked { name, src, lint_as }, entry));
    }
    out
}

/// The fixed form of an [`IGNORED`] sample has the error that shows `tsc` reads it now.
fn check_ignored(
    file: &Checked,
    wanted: &str,
    why: &str,
    diags: &[&Diag],
    failures: &mut Vec<String>,
) {
    if diags.iter().any(|d| d.code == wanted) {
        eprintln!("{}: tsc reports TS{wanted}: {why}", file.name);
    } else {
        let got: Vec<String> = diags.iter().map(|d| d.to_string()).collect();
        failures.push(format!(
            "{}: the fix should make tsc report TS{wanted} ({why}), but it reports {got:?}",
            file.name
        ));
    }
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// Runs `diagnostics.mjs` on the project.
fn run_tsc(oracle: &Path, project: &Path) -> Vec<Diag> {
    let out = command("node")
        .arg(oracle.join("diagnostics.mjs"))
        .arg(project.join("tsconfig.json"))
        .output()
        .expect("run node");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "diagnostics.mjs failed:\n{stderr}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            let mut parts = line.splitn(4, '\t');
            let mut next = || parts.next().unwrap_or_default().to_string();
            let (file, line, code, message) = (next(), next(), next(), next());
            let line = line.parse().unwrap_or(0);
            Diag {
                file,
                line,
                code,
                message,
            }
        })
        .collect()
}

/// A top-level declaration: its lines and the findings in it.
struct Decl<'f> {
    lines: (usize, usize),
    findings: Vec<&'f Finding>,
}

/// Holds a fixture to the lint a declaration at a time: what `tsc` reports is in declarations
/// the lint reports too, and a declaration with a rule `tsc` rejects has an error.
fn check_case(file: &Checked, diags: &[&Diag], failures: &mut Vec<String>) {
    let module = parse(&file.lint_as, &file.src);
    let findings = lint_module(&file.lint_as, &file.src, &module);
    let mut decls: Vec<Decl> = module
        .items
        .iter()
        .map(|item| Decl {
            lines: (
                line_of(&file.src, item.span.lo),
                line_of(&file.src, item.span.hi),
            ),
            findings: vec![],
        })
        .collect();
    for f in &findings {
        let line = line_of(&file.src, f.span.lo);
        match decls
            .iter_mut()
            .find(|d| d.lines.0 <= line && line <= d.lines.1)
        {
            Some(d) => d.findings.push(f),
            // A comment before the declarations (a pragma): only a rejected claim needs the
            // declaration `tsc` reports.
            None if !matches!(claim(f.code), Claim::Rejected) => {}
            None => failures.push(format!(
                "{}:{line}: a finding outside declarations",
                file.name
            )),
        }
    }
    for d in diags {
        match decls
            .iter()
            .find(|x| x.lines.0 <= d.line && d.line <= x.lines.1)
        {
            Some(decl) if decl.findings.is_empty() => match known_gap(d) {
                Some(why) => eprintln!("known gap: {d}: {why}"),
                None => failures.push(format!("tsc rejects code the lint passes: {d}")),
            },
            Some(_) => {}
            None => failures.push(format!("tsc reports outside declarations: {d}")),
        }
    }
    for (_, line, _) in KNOWN_GAPS.iter().filter(|(f, _, _)| *f == file.name) {
        if !diags.iter().any(|d| d.line == *line) {
            failures.push(format!(
                "{}:{line}: a known gap that tsc accepts now: remove it from KNOWN_GAPS",
                file.name
            ));
        }
    }
    for decl in &decls {
        let rejected = decl
            .findings
            .iter()
            .find(|f| matches!(claim(f.code), Claim::Rejected));
        let (lo, hi) = decl.lines;
        let has_error = diags.iter().any(|d| lo <= d.line && d.line <= hi);
        if let Some(f) = rejected.filter(|_| !has_error) {
            failures.push(format!(
                "{}:{lo}-{hi}: the lint reports `{}`, which tsc should reject, but tsc accepts it",
                file.name, f.code
            ));
        }
    }
    eprintln!(
        "{}: {} declarations, {} findings, {} tsc diagnostics",
        file.name,
        decls.len(),
        findings.len(),
        diags.len()
    );
}

fn known_gap(d: &Diag) -> Option<&'static str> {
    KNOWN_GAPS
        .iter()
        .find(|(file, line, _)| *file == d.file && *line == d.line)
        .map(|(_, _, why)| *why)
}

/// Every line of a rejected sample with a finding has a `tsc` error.
fn check_rejected(file: &Checked, diags: &[&Diag], failures: &mut Vec<String>) {
    let findings = lint_source(&file.lint_as, &file.src);
    let mut lines: Vec<(usize, &str)> = findings
        .iter()
        .map(|f| (line_of(&file.src, f.span.lo), f.code))
        .collect();
    lines.dedup();
    let mut proved = vec![];
    for (line, code) in lines {
        let at_line: Vec<&str> = diags
            .iter()
            .filter(|d| d.line == line)
            .map(|d| d.code.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if at_line.is_empty() {
            failures.push(format!(
                "{}:{line}: the lint reports `{code}`, but tsc accepts the line",
                file.name
            ));
        } else {
            proved.push(format!("{line} (TS{})", at_line.join(", TS")));
        }
    }
    eprintln!("{}: tsc rejects lines {}", file.name, proved.join(", "));
}

/// `Command::new(program)` for a test's child process. On Windows, when this test process has no
/// console (a CI agent, a background shell), the child gets a hidden console instead of opening a
/// window of its own. In a terminal it shares the terminal's console as before, so Ctrl+C still
/// reaches it.
fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let cmd = std::process::Command::new(program);
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleWindow() -> *mut std::ffi::c_void;
        }
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // SAFETY: takes no arguments; returns this process's console window or null.
        if unsafe { GetConsoleWindow() }.is_null() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    };
    cmd
}
