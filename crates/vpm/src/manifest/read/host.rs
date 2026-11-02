//! Decoding `host` of `package.vlt` ([`crate::host`]), with each check located at the key or
//! value it rejects.

use super::{Reader, Value};
use crate::host::{check_entry, check_scaffold, check_target, HostConfig};

impl Reader<'_> {
    /// `host: { target: string; condition: string; entry: string; scaffold?: Record<string,
    /// string> }`.
    pub(super) fn host(&mut self, value: &Value) -> Option<HostConfig> {
        let mut config = HostConfig::default();
        let mut scaffold = vec![];
        for (key, v) in self.object(value, "`host`")? {
            match key.name.as_str() {
                "target" => {
                    if let Some(s) = self.string(v, "target") {
                        self.check(check_target(s), v.span);
                        config.target = s.to_string();
                    }
                }
                "condition" => {
                    if let Some(s) = self.string(v, "condition") {
                        self.check(crate::imports::check_host_condition(s), v.span);
                        config.condition = s.to_string();
                    }
                }
                "entry" => {
                    if let Some(s) = self.string(v, "entry") {
                        self.check(check_entry(s), v.span);
                        config.entry = s.to_string();
                    }
                }
                "scaffold" => {
                    for (path, file) in self.object(v, "`scaffold`").unwrap_or_default() {
                        if let Some(f) = self.string(file, &path.name) {
                            scaffold.push((path.clone(), f.to_string(), file.span));
                        }
                    }
                }
                _ => self.unknown_key(key, crate::manifest::schema::HOST),
            }
        }
        for (field, given) in [
            ("target", &config.target),
            ("condition", &config.condition),
            ("entry", &config.entry),
        ] {
            if given.is_empty() {
                self.error(format!("`host` needs `{field}`"), value.span);
            }
        }
        for (path, file, span) in scaffold {
            self.check(check_scaffold(&path.name, &file, &config.entry), path.span.to(span));
            config.scaffold.insert(path.name, file);
        }
        Some(config)
    }
}
