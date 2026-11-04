//! Decoding `lint` of `package.vlt` ([`crate::manifest::LintConfig`]).

use super::{Reader, Value, ValueKind};
use crate::manifest::schema;
use crate::manifest::{LintConfig, LintLevel};

impl Reader<'_> {
    /// `lint: { perf?: LintLevel, rules?: Record<string, LintLevel> }`.
    pub(super) fn lint(&mut self, value: &Value) -> Option<LintConfig> {
        let mut config = LintConfig::default();
        for (key, v) in self.object(value, "`lint`")? {
            match key.name.as_str() {
                "perf" => config.perf = self.lint_level(v, "`lint.perf`"),
                "rules" => {
                    for (code, level) in self.object(v, "`lint.rules`").unwrap_or_default() {
                        let what = format!("the level of `{}`", code.name);
                        if let Some(level) = self.lint_level(level, &what) {
                            config.rules.insert(code.name.clone(), level);
                        }
                    }
                }
                _ => self.unknown_key(key, schema::LINT),
            }
        }
        Some(config)
    }

    /// One of `"off"`, `"hint"`, `"warn"`, `"deny"`.
    fn lint_level(&mut self, value: &Value, what: &str) -> Option<LintLevel> {
        let ValueKind::Str(s) = &value.kind else {
            self.error(
                format!("{what} must be a string, not {}", value.kind.describe()),
                value.span,
            );
            return None;
        };
        let level = LintLevel::parse(s);
        if level.is_none() {
            self.error(
                format!("{what} must be \"off\", \"hint\", \"warn\" or \"deny\", not \"{s}\""),
                value.span,
            );
        }
        level
    }
}
