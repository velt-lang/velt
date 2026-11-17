//! The failed tests of a nextest JUnit report, for the job summary of a CI run.

use std::path::Path;

/// nextest's profile for a CI run (.config/nextest.toml), from `VELT_NEXTEST_PROFILE`: `ci`
/// (pull requests, `main`, the nightly), which reports every failure, or `queue` (the merge
/// queue's `check` job), which stops at the first. Both write `target/nextest/<profile>/junit.xml`.
pub fn ci_profile(var: Option<String>) -> Result<&'static str, String> {
    match var.as_deref() {
        None | Some("") | Some("ci") => Ok("ci"),
        Some("queue") => Ok("queue"),
        Some(other) => Err(format!(
            "VELT_NEXTEST_PROFILE={other}: not a CI profile (`ci` or `queue`)"
        )),
    }
}

/// Lists the failed tests of a nextest JUnit report in the job summary (`$GITHUB_STEP_SUMMARY`),
/// so a failure is attributable from the run's page. Best effort: without the report or the
/// summary file there is nothing to add to nextest's own output.
pub fn summarize_failed_tests(report: &Path) {
    let (Some(summary), Ok(xml)) = (
        std::env::var_os("GITHUB_STEP_SUMMARY"),
        std::fs::read_to_string(report),
    ) else {
        return;
    };
    let failed = junit_failures(&xml);
    if failed.is_empty() {
        return;
    }
    let mut text = format!(
        "### {} failed tests ({})\n\n",
        failed.len(),
        std::env::consts::OS
    );
    for test in &failed {
        text += &format!("- `{test}`\n");
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(summary)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// The test cases a JUnit report records as failed (a `<failure>` or `<error>` child), as
/// `classname name` (nextest: `velt_sema::scaling sema_time_grows_linearly`).
fn junit_failures(xml: &str) -> Vec<String> {
    let mut failed = vec![];
    for case in xml.split("<testcase ").skip(1) {
        if !(case.contains("<failure") || case.contains("<error")) {
            continue;
        }
        let tag = format!(" {}", &case[..case.find('>').unwrap_or(case.len())]);
        let attr = |name: &str| {
            let key = format!(" {name}=\"");
            let Some(start) = tag.find(&key).map(|i| i + key.len()) else {
                return String::new();
            };
            let value = &tag[start..];
            let value = &value[..value.find('"').unwrap_or(value.len())];
            value
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        };
        failed.push(format!("{} {}", attr("classname"), attr("name")));
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::{ci_profile, junit_failures};

    /// Every profile `ci_profile` picks is in .config/nextest.toml with a JUnit report, and only
    /// the merge queue's stops at the first failure.
    #[test]
    fn the_ci_profiles_exist_and_only_the_queue_stops_early() {
        let config = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.config/nextest.toml"),
        )
        .unwrap()
        .replace("\r\n", "\n");
        let section = |name: &str| {
            let head = format!("\n[profile.{name}]\n");
            let start = config.find(&head).unwrap_or_else(|| panic!("no {head}")) + head.len();
            let end = config[start..]
                .find("\n[")
                .map_or(config.len(), |e| start + e);
            config[start..end].to_string()
        };
        for (var, profile, fail_fast) in [
            (None, "ci", "fail-fast = false"),
            (Some("ci"), "ci", "fail-fast = false"),
            (Some("queue"), "queue", "fail-fast = true"),
        ] {
            assert_eq!(ci_profile(var.map(String::from)), Ok(profile));
            assert!(section(profile).contains(fail_fast), "[profile.{profile}]");
            let junit = section(&format!("{profile}.junit"));
            assert!(junit.contains("path = \"junit.xml\""), "{profile}");
        }
        assert!(ci_profile(Some("queeu".into())).is_err());
    }

    #[test]
    fn failed_cases_are_named_by_class_and_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1" errors="1">
    <testsuite name="velt_sema::scaling" tests="2" failures="1">
        <testcase name="sema_time_grows_linearly" classname="velt_sema::scaling" time="17.5">
            <failure type="test failure">thread panicked: &lt;grew&gt;</failure>
        </testcase>
        <testcase name="instantiation_work_grows_linearly" classname="velt_sema::scaling" time="1.0"/>
    </testsuite>
    <testsuite name="xtask" tests="1" errors="1">
        <testcase name="a &amp; b &lt;T&gt;" classname="xtask" time="0.1">
            <error type="test abort">SIGSEGV</error>
        </testcase>
    </testsuite>
</testsuites>"#;
        assert_eq!(
            junit_failures(xml),
            [
                "velt_sema::scaling sema_time_grows_linearly",
                "xtask a & b <T>"
            ]
        );
        assert!(junit_failures("<testsuites/>").is_empty());
    }
}
