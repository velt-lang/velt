//! The failed tests of a nextest JUnit report, for the job summary of a CI run.

use std::path::Path;

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
    use super::junit_failures;

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
