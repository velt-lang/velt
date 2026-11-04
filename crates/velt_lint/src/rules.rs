//! The registry of lint rules, and how loud each is in a package (`lint` in `package.vlt`).

use std::collections::BTreeMap;

use velt_common::Severity;
use vpm::manifest::{LintConfig, LintLevel};

/// How a rule finds what it reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// From the syntax tree and the checker's types: cheap, runs in `velt check` and the editor.
    Pattern,
    /// From what the optimizer could not do: runs with `velt check --perf`, which optimizes the
    /// program (no code generation).
    Evidence,
}

/// One `perf/…` rule.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    /// `perf/number-double`.
    pub code: &'static str,
    pub kind: Kind,
    /// What it reports, in one line (for `velt help` and the docs).
    pub summary: &'static str,
}

/// Every `perf/…` rule. Each is backed by a measurement (the design's Rules table) and a test
/// that fails without it.
pub const PERF_RULES: &[Rule] = &[
    Rule {
        code: "perf/class-as-data",
        kind: Kind::Pattern,
        summary: "a class that only holds data: an object type stores the fields inline",
    },
    Rule {
        code: "perf/dynamic-record",
        kind: Kind::Pattern,
        summary: "a `Record<string, any>` parameter read with constant keys: an object type \
                  makes each read a load",
    },
    Rule {
        code: "perf/foreach-capture",
        kind: Kind::Pattern,
        summary: "a `forEach` callback that assigns an outer variable, which then lives in a \
                  heap cell: `for…of` runs the body in place",
    },
    Rule {
        code: "perf/number-double",
        kind: Kind::Evidence,
        summary: "a `number` in a loop that the optimizer keeps as a double, with why",
    },
];

/// The `ts-compat/…` code of a `velt_tscompat` rule (`bool-type` → `ts-compat/bool-type`).
pub fn ts_compat_code(rule: &str) -> String {
    format!("ts-compat/{rule}")
}

/// Whether `code` names a rule: a `perf/…` rule, a `ts-compat/…` one, or a family's wildcard
/// (`perf/*`).
pub fn is_known(code: &str) -> bool {
    if let Some(family) = code.strip_suffix("/*") {
        return family == "perf" || family == "ts-compat";
    }
    PERF_RULES.iter().any(|r| r.code == code)
        || code
            .strip_prefix("ts-compat/")
            .is_some_and(|rule| velt_tscompat::RULES.contains(&rule))
}

/// The rule `code`.
pub fn rule(code: &str) -> Option<&'static Rule> {
    PERF_RULES.iter().find(|r| r.code == code)
}

/// How a diagnostic's code is shown as its last note: `perf/number-double` →
/// `perf(number-double)`, as `ts-compat(<rule>)` always was.
pub fn code_note(code: &str) -> String {
    match code.split_once('/') {
        Some((family, rule)) => format!("{family}({rule})"),
        None => code.to_string(),
    }
}

/// The levels of a package's rules.
#[derive(Clone, Debug, Default)]
pub struct Levels {
    perf: LintLevel,
    rules: BTreeMap<String, LintLevel>,
}

impl Levels {
    /// The levels `lint` sets (none: every `perf/…` rule is a hint).
    pub fn new(config: Option<&LintConfig>) -> Self {
        let Some(config) = config else {
            return Levels::default();
        };
        Levels {
            perf: config.perf.unwrap_or_default(),
            rules: config.rules.clone(),
        }
    }

    /// The level of the `perf/…` rule `code`.
    pub fn perf(&self, code: &str) -> LintLevel {
        self.rules.get(code).copied().unwrap_or(self.perf)
    }

    /// The level `lint.rules` sets for a `ts-compat/…` code, if any (otherwise the finding's own
    /// severity holds).
    pub fn ts_compat(&self, code: &str) -> Option<LintLevel> {
        self.rules.get(code).copied()
    }

    /// Whether some `perf/…` rule of `kind` is at `level` or louder: plain `velt check` runs the
    /// pattern rules only when one is a warning or an error.
    pub fn any_perf(&self, kind: Kind, level: LintLevel) -> bool {
        PERF_RULES
            .iter()
            .filter(|r| r.kind == kind)
            .any(|r| self.perf(r.code) >= level)
    }

    /// The codes in `lint.rules` that name no rule, for a warning.
    pub fn unknown_codes(&self) -> Vec<&str> {
        self.rules
            .keys()
            .map(String::as_str)
            .filter(|c| !is_known(c))
            .collect()
    }
}

/// The severity of a finding at `level` (`None`: not reported).
pub fn severity(level: LintLevel) -> Option<Severity> {
    match level {
        LintLevel::Off => None,
        LintLevel::Hint => Some(Severity::Note),
        LintLevel::Warn => Some(Severity::Warning),
        LintLevel::Deny => Some(Severity::Error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_win_over_the_family() {
        let mut config = LintConfig {
            perf: Some(LintLevel::Warn),
            ..LintConfig::default()
        };
        config
            .rules
            .insert("perf/number-double".into(), LintLevel::Off);
        config.rules.insert("perf/nope".into(), LintLevel::Deny);
        let levels = Levels::new(Some(&config));
        assert_eq!(levels.perf("perf/number-double"), LintLevel::Off);
        assert_eq!(levels.perf("perf/foreach-capture"), LintLevel::Warn);
        assert!(levels.any_perf(Kind::Pattern, LintLevel::Warn));
        assert!(!levels.any_perf(Kind::Evidence, LintLevel::Hint));
        assert_eq!(levels.unknown_codes(), ["perf/nope"]);
        assert_eq!(
            Levels::new(None).perf("perf/number-double"),
            LintLevel::Hint
        );
    }

    #[test]
    fn codes() {
        assert!(is_known("perf/number-double") && is_known("ts-compat/bool-type"));
        assert!(is_known("perf/*") && !is_known("ts-compat/nope") && !is_known("bool-type"));
        assert_eq!(code_note("perf/number-double"), "perf(number-double)");
        assert_eq!(
            code_note(&ts_compat_code("bool-type")),
            "ts-compat(bool-type)"
        );
    }
}
