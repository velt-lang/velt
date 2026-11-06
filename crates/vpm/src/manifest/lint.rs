//! The `lint` object of `package.velt`: how loud the lint rules are in the package's own modules
//! (docs/internals/design/lint-profiles.md §5).
//!
//! ```ts ignore
//! lint: {
//!   perf: "warn",                                   // every `perf/…` rule
//!   rules: { "perf/number-double": "off" },         // one rule, which wins
//! },
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

/// How loud a rule is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LintLevel {
    /// Not reported.
    Off,
    /// Shown by `velt check --perf` and in the editor (as a hint), not by plain `velt check`.
    #[default]
    Hint,
    /// A warning wherever the rule runs.
    Warn,
    /// An error: `velt check` fails.
    Deny,
}

impl LintLevel {
    /// The levels as `package.velt` writes them.
    pub const NAMES: [&'static str; 4] = ["off", "hint", "warn", "deny"];

    /// The level named `name` (one of [`Self::NAMES`]).
    pub fn parse(name: &str) -> Option<LintLevel> {
        match name {
            "off" => Some(LintLevel::Off),
            "hint" => Some(LintLevel::Hint),
            "warn" => Some(LintLevel::Warn),
            "deny" => Some(LintLevel::Deny),
            _ => None,
        }
    }

    /// The level's name in `package.velt`.
    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }
}

/// The `lint` object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintConfig {
    /// `perf`: the level of every `perf/…` rule (default [`LintLevel::Hint`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<LintLevel>,
    /// `rules`: levels of single rules by code (`"perf/number-double"`), which win over `perf`.
    /// The codes are checked by the tools that run the rules, which know them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<String, LintLevel>,
}
