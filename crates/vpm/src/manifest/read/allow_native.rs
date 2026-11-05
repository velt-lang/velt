//! Decoding `allowNative` (crate::manifest::allow_native): an array of package names and
//! `{ name, git }` / `{ name, url }` entries, each checked where it is written.

use super::{Reader, Value, ValueKind};
use crate::manifest::allow_native::{check_from, AllowNative, AllowedFrom};
use crate::manifest::schema;

impl Reader<'_> {
    /// `allowNative: (string | { name: string; git?: string; url?: string })[]`, in the order
    /// written.
    pub(super) fn allow_native(&mut self, value: &Value) -> Vec<AllowNative> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!(
                    "`allowNative` must be an array, not {}",
                    value.kind.describe()
                ),
                value.span,
            );
            return Vec::new();
        };
        let mut out = Vec::new();
        for elem in elems {
            match &elem.kind {
                ValueKind::Str(s) => out.push(AllowNative::named(s.clone())),
                ValueKind::Object(fields) => {
                    if let Some(entry) = self.allow_native_entry(fields, elem) {
                        out.push(entry);
                    }
                }
                other => self.error(
                    format!(
                        "`allowNative` entries must be package names or `{{ name, git }}` / `{{ name, url }}` objects, not {}",
                        other.describe()
                    ),
                    elem.span,
                ),
            }
        }
        out
    }

    fn allow_native_entry(
        &mut self,
        fields: &[(velt_syntax::ast::Ident, Value)],
        elem: &Value,
    ) -> Option<AllowNative> {
        let mut name = None;
        let mut from = vec![];
        for (key, v) in fields {
            match key.name.as_str() {
                "name" => name = self.string(v, "name").map(str::to_string),
                "git" => {
                    if let Some(url) = self.string(v, "git") {
                        let git = AllowedFrom::Git(url.to_string());
                        self.check(check_from(&git), v.span);
                        from.push(git);
                    }
                }
                "url" => {
                    if let Some(prefix) = self.string(v, "url") {
                        let url = AllowedFrom::Url(prefix.to_string());
                        self.check(check_from(&url), v.span);
                        from.push(url);
                    }
                }
                _ => self.unknown_key(key, schema::ALLOW_NATIVE),
            }
        }
        let Some(name) = name else {
            self.error("an `allowNative` entry needs a `name`", elem.span);
            return None;
        };
        if from.len() != 1 {
            self.error(
                format!(
                    "the `allowNative` entry for `{name}` needs exactly one of `git` (a repository) and `url` (a URL prefix); a bare \"{name}\" allows it from the registry"
                ),
                elem.span,
            );
            return None;
        }
        Some(AllowNative {
            name,
            from: from.remove(0),
        })
    }
}
