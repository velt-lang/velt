//! Decoding conditional imports ([`crate::imports`]): a package's `imports` field, and the
//! standard library's own map, `std/imports.vlt` (`export const imports: Imports = { … }`), with
//! the manifest's reader: the same data subset, bounds and checks.

use std::collections::BTreeMap;

use velt_common::{Diagnostics, FileId};

use super::{read_data, Reader, Shape, Value, ValueKind};
use crate::imports::{
    check_condition, check_key, check_target, ImportMap, Variant, DEFAULT, STD_IMPORTS_FILE,
};

const STD_IMPORTS_SHAPE: Shape = Shape {
    file: STD_IMPORTS_FILE,
    binding: "imports",
    type_name: "Imports",
    message: "std's imports file must be one `export const imports: Imports = { … }`",
};

/// Read and validate the text of `std/imports.vlt`. Every problem is a diagnostic in `file`.
pub fn read_std_imports(file: FileId, src: &str) -> Result<ImportMap, Diagnostics> {
    read_data(file, src, &STD_IMPORTS_SHAPE, |r, v| r.imports(v, false)).map(|(map, _)| map)
}

impl Reader<'_> {
    /// `imports`: `#name` → a file or `{ condition: file, … }`; `src`: the files are a package's
    /// modules, below `./src/`.
    pub(super) fn imports(&mut self, value: &Value, src: bool) -> ImportMap {
        let mut out = BTreeMap::new();
        for (key, v) in self.object(value, "`imports`").unwrap_or_default() {
            self.check(check_key(&key.name), key.span);
            let variants = match &v.kind {
                ValueKind::Str(target) => {
                    self.check(check_target(target, src), v.span);
                    vec![Variant {
                        condition: DEFAULT.into(),
                        target: target.clone(),
                    }]
                }
                ValueKind::Object(fields) => self.variants(&key.name, fields, v, src),
                other => {
                    self.error(
                        format!(
                            "`{}` must be a file or an object of conditions, not {}",
                            key.name,
                            other.describe()
                        ),
                        v.span,
                    );
                    continue;
                }
            };
            out.insert(key.name.clone(), variants);
        }
        out
    }

    fn variants(
        &mut self,
        name: &str,
        fields: &[(velt_syntax::ast::Ident, Value)],
        value: &Value,
        src: bool,
    ) -> Vec<Variant> {
        if fields.is_empty() {
            self.error(format!("`{name}` has no variants"), value.span);
        }
        let mut out = vec![];
        let mut after_default = false;
        for (condition, v) in fields {
            if after_default {
                self.error(
                    format!(
                        "`{}` comes after `{DEFAULT}`, which every build has: it never applies (put `{DEFAULT}` last)",
                        condition.name
                    ),
                    condition.span,
                );
            }
            after_default |= condition.name == DEFAULT;
            self.check(check_condition(&condition.name), condition.span);
            let Some(target) = self.string(v, &condition.name) else {
                continue;
            };
            self.check(check_target(target, src), v.span);
            out.push(Variant {
                condition: condition.name.clone(),
                target: target.to_string(),
            });
        }
        out
    }
}
