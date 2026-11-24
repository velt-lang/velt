//! `cargo xtask main-plan`: what ci.yml runs after a push to `main` and in its scheduled run
//! (the `main-plan` job). Two kinds of jobs follow it:
//!
//! - `main (ubuntu-latest)`, on every push: it builds and saves Linux's build cache (and
//!   `clang-objects` saves what the run that tested the commit compiled);
//! - the gate, `main (<os>)` (.github/workflows/main-gate.yml): per OS a test job per test shard
//!   (`os::TEST_SHARDS`) and the goldens' build and shards (queue-goldens.yml, `os::GOLDEN_SHARDS`).
//!
//! Which OSes the gate runs on ([`decide`]):
//!
//! - the scheduled run: Windows and macOS, every day;
//! - a push whose commit is no longer `main`'s tip when the plan runs: none. A newer commit landed
//!   (the next PR of a train, or a merge right after), and its own run checks the code of both;
//! - a push whose commit a passing run tested: the merge queue (a `merge_group` run on the same
//!   commit) or a merge train (a dispatched run on a `train-*` branch whose head has the same
//!   tree: the train merged its PRs onto this base and tested exactly these files). Then only
//!   when the change touches the toolchain or the dependencies, Windows and macOS, to refresh
//!   their build caches (`os::after_merge_reason`, planned from the base that run tested on:
//!   `HEAD^1` for the queue, the train's base for a train);
//! - any other push (a direct push, an admin merge that no train tested): every OS.

use std::path::Path;

use crate::graph::Graph;
use crate::os;
use crate::plan::Plan;

const WINDOWS_MACOS: &[&str] = &["windows-latest", "macos-latest"];

/// What started the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Push,
    Schedule,
}

/// Which passing run tested the pushed commit, if one did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tested {
    Queue,
    Train,
    No,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Decision {
    /// Whether `main (ubuntu-latest)` builds and saves Linux's build cache.
    pub cache: bool,
    /// The OSes the gate runs on.
    pub gate: Vec<&'static str>,
    pub reason: String,
}

/// The jobs for a run started by `event`. `superseded`: the pushed commit is no longer `main`'s
/// tip. `after_merge`: why the change needs Windows' and macOS' build caches refreshed, planned
/// from the base the testing run tested on.
pub fn decide(
    event: Event,
    tested: Tested,
    superseded: bool,
    after_merge: Option<&str>,
) -> Decision {
    let (gate, reason): (&[&'static str], String) = match (event, tested) {
        (Event::Schedule, _) => (
            WINDOWS_MACOS,
            "the scheduled run: the whole gate on Windows and macOS".into(),
        ),
        _ if superseded => (
            &[],
            "a newer commit is on main: its run checks this one's code too".into(),
        ),
        (Event::Push, Tested::Queue | Tested::Train) => {
            let by = match tested {
                Tested::Queue => "the merge queue",
                _ => "a merge train (the same tree)",
            };
            match after_merge {
                Some(why) => (WINDOWS_MACOS, format!("{by} tested it, but {why}")),
                None => (&[], format!("{by} tested it on every OS it needs")),
            }
        }
        (Event::Push, Tested::No) => (
            os::QUEUE_OSES,
            "no passing merge queue or train run tested this commit: every OS".into(),
        ),
    };
    Decision {
        cache: event == Event::Push,
        gate: gate.to_vec(),
        reason,
    }
}

/// The `$GITHUB_OUTPUT` lines for `d`: `cache`, `gate_os` and the shards of each of its OSes
/// (`test_shards`, `golden_shards`), and how much of the VIR identity the golden shards compare
/// (`vir_identity`: the whole gate's plan, as the queue's `--full` gives it).
pub fn github_output_text(d: &Decision) -> String {
    let quoted: Vec<String> = d.gate.iter().map(|os| format!("\"{os}\"")).collect();
    format!(
        "cache={}\ngate_os=[{}]\ntest_shards={}\ngolden_shards={}\nvir_identity={}\n",
        d.cache,
        quoted.join(","),
        os::test_shards_json(&d.gate),
        os::golden_shards_json(&d.gate),
        Plan::everything("--full").vir_identity_queue(),
    )
}

/// `cargo xtask main-plan --event push|schedule [--tested queue|train|no] [--base <rev>]
/// [--superseded true|false] [--github]`.
pub fn main(root: &Path, args: &[String]) -> Result<(), String> {
    let (mut event, mut tested, mut base) = (None, Tested::No, None);
    let (mut superseded, mut github) = (false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--event" => {
                event = Some(match value("--event")?.as_str() {
                    "push" => Event::Push,
                    "schedule" => Event::Schedule,
                    other => return Err(format!("--event {other}: expected push or schedule")),
                })
            }
            "--tested" => {
                tested = match value("--tested")?.as_str() {
                    "queue" => Tested::Queue,
                    "train" => Tested::Train,
                    "no" | "" => Tested::No,
                    other => return Err(format!("--tested {other}: expected queue, train or no")),
                }
            }
            "--base" => base = Some(value("--base")?).filter(|b| !b.is_empty()),
            "--superseded" => {
                superseded = match value("--superseded")?.as_str() {
                    "true" => true,
                    "false" | "" => false,
                    other => return Err(format!("--superseded {other}: expected true or false")),
                }
            }
            "--github" => github = true,
            other => return Err(format!("main-plan: unknown option `{other}`")),
        }
    }
    let event = event.ok_or("main-plan needs --event push or --event schedule")?;
    // Only a tested push asks what changed; without a base nobody knows, so the caches refresh.
    let after_merge = match (event, tested) {
        (Event::Push, Tested::Queue | Tested::Train) => after_merge(root, base.as_deref()),
        _ => None,
    };
    let d = decide(event, tested, superseded, after_merge.as_deref());
    let text = github_output_text(&d);
    print!("{}\n{text}", d.reason);
    if github {
        append("GITHUB_OUTPUT", &text)?;
        append("GITHUB_STEP_SUMMARY", &format!("main-plan: {}\n", d.reason))?;
    }
    Ok(())
}

/// Why the change since `base` refreshes Windows' and macOS' build caches.
fn after_merge(root: &Path, base: Option<&str>) -> Option<String> {
    let plan = match base.map(|b| crate::changes::changes(root, b)) {
        Some(Ok(c)) => match Graph::load(root) {
            Ok(graph) => Plan::for_changes(&graph, &c.paths, &c.comment_only),
            Err(e) => Plan::unknown_changes(e),
        },
        Some(Err(e)) => Plan::unknown_changes(e),
        None => Plan::unknown_changes("no base the testing run tested on"),
    };
    plan.after_merge
}

fn append(var: &str, text: &str) -> Result<(), String> {
    let Some(path) = std::env::var_os(var) else {
        return Ok(());
    };
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(|e| format!("{var}: {e}"))?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("{var}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[&str] = &["ubuntu-latest", "windows-latest", "macos-latest"];

    #[test]
    fn the_scheduled_run_checks_windows_and_macos_and_saves_no_linux_cache() {
        for tested in [Tested::No, Tested::Queue] {
            let d = decide(Event::Schedule, tested, true, None);
            assert_eq!(d.gate, WINDOWS_MACOS);
            assert!(!d.cache);
        }
    }

    #[test]
    fn a_commit_nothing_tested_runs_the_gate_on_every_os() {
        let d = decide(Event::Push, Tested::No, false, None);
        assert_eq!(d.gate, ALL);
        assert!(d.cache);
    }

    #[test]
    fn a_commit_the_queue_or_a_train_tested_runs_the_gate_only_for_the_caches() {
        for tested in [Tested::Queue, Tested::Train] {
            let d = decide(Event::Push, tested, false, None);
            assert!(d.gate.is_empty(), "{tested:?}");
            assert!(d.cache);
            let why = "Cargo.lock changed: refresh the build cache";
            let d = decide(Event::Push, tested, false, Some(why));
            assert_eq!(d.gate, WINDOWS_MACOS, "{tested:?}");
            assert!(d.reason.contains(why), "{}", d.reason);
        }
    }

    #[test]
    fn a_commit_a_newer_one_replaced_on_main_skips_the_gate_but_saves_the_cache() {
        for tested in [Tested::No, Tested::Queue, Tested::Train] {
            let d = decide(Event::Push, tested, true, Some("Cargo.lock changed"));
            assert!(d.gate.is_empty(), "{tested:?}");
            assert!(d.cache);
        }
    }

    #[test]
    fn the_outputs_give_each_gate_os_its_shards() {
        let text = github_output_text(&decide(Event::Push, Tested::No, false, None));
        assert!(text.starts_with("cache=true\n"), "{text}");
        assert!(
            text.contains(r#"gate_os=["ubuntu-latest","windows-latest","macos-latest"]"#),
            "{text}"
        );
        let line = |name: &str| {
            let prefix = format!("{name}=");
            let found = text.lines().find_map(|l| l.strip_prefix(prefix.as_str()));
            found.unwrap_or_default().to_string()
        };
        assert!(line("test_shards").contains(r#""windows-latest":["1/3","2/3","3/3"]"#));
        assert!(line("golden_shards").contains(r#""windows-latest":["1/4","2/4","3/4","4/4"]"#));
        // The VIR identity's sample, as in the queue, never every golden (`all`).
        assert_eq!(line("vir_identity"), "sample", "{text}");
        let none = github_output_text(&decide(Event::Push, Tested::Queue, false, None));
        assert!(
            none.contains("gate_os=[]\ntest_shards={}\ngolden_shards={}\n"),
            "{none}"
        );
        let nightly = github_output_text(&decide(Event::Schedule, Tested::No, false, None));
        assert!(nightly.starts_with("cache=false\n"), "{nightly}");
        assert!(!nightly.contains("ubuntu"), "{nightly}");
    }

    #[test]
    fn a_tested_push_without_a_base_refreshes_the_caches() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(after_merge(root, None).is_some());
        assert!(after_merge(root, Some("no-such-revision-xyz")).is_some());
    }

    #[test]
    fn the_command_line_is_checked() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(main(root, &args(&[])).is_err());
        assert!(main(root, &args(&["--event", "pull_request"])).is_err());
        assert!(main(root, &args(&["--event", "push", "--tested", "maybe"])).is_err());
        assert!(main(root, &args(&["--event", "push", "--bogus"])).is_err());
        assert!(main(root, &args(&["--event", "push", "--superseded", "yes"])).is_err());
        let ok = [
            "--event",
            "push",
            "--tested",
            "no",
            "--base",
            "",
            "--superseded",
            "true",
        ];
        assert!(main(root, &args(&ok)).is_ok());
    }
}
