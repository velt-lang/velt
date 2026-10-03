//! Decoding `tsCompat` of `package.vlt`: the folders whose modules stay in the TypeScript/Velt
//! common subset (docs/internals/design/tsx.md "Sharing components with the client").
//!
//! The reader checks what the text says (relative paths inside the package, each folder once);
//! whether the folders exist is checked by the tools that lint them, against the package on
//! disk ([`missing_ts_compat_dirs`]).

use std::path::Path;

use velt_common::{Diagnostic, Diagnostics, FileId, Severity};

use super::{Reader, Value, ValueKind};

impl Reader<'_> {
    /// `tsCompat: string[]`, in the order written.
    pub(super) fn ts_compat(&mut self, value: &Value) -> Vec<String> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!("`tsCompat` must be an array, not {}", value.kind.describe()),
                value.span,
            );
            return vec![];
        };
        if elems.is_empty() {
            self.error("`tsCompat` is empty; remove the field instead", value.span);
        }
        let mut dirs: Vec<String> = vec![];
        for elem in elems {
            let ValueKind::Str(dir) = &elem.kind else {
                self.error(
                    format!(
                        "`tsCompat` entries must be strings, not {}",
                        elem.kind.describe()
                    ),
                    elem.span,
                );
                continue;
            };
            if let Err(why) = check_dir(dir).and_then(|()| check_overlap(dir, &dirs)) {
                self.error(why, elem.span);
                continue;
            }
            dirs.push(dir.clone());
        }
        dirs
    }
}

/// A `/`-separated path to a folder inside the package. Checked on the text, like `entry`, so a
/// manifest means the same on every OS.
fn check_dir(dir: &str) -> Result<(), String> {
    let inside = !dir.contains(['\\', ':'])
        && dir
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..");
    if inside {
        Ok(())
    } else {
        Err(format!(
            "`tsCompat` folder `{dir}` must be a `/`-separated path inside the package, relative to \
             its root (such as `src/models`: no `.` or `..` parts, no leading or trailing `/`)"
        ))
    }
}

/// Each folder once: not listed before, and not inside (or around) a folder listed before.
/// Names are compared ignoring case: `src/Models` is `src/models` on case-insensitive file
/// systems (macOS and Windows by default), and two entries that differ only in case are
/// suspicious on any OS, so the manifest means the same everywhere.
fn check_overlap(dir: &str, earlier: &[String]) -> Result<(), String> {
    let key = |d: &str| d.to_lowercase();
    let within = |inner: &str, outer: &str| {
        key(inner)
            .strip_prefix(&key(outer))
            .is_some_and(|rest| rest.starts_with('/'))
    };
    let same = |e: &&String| key(e) == key(dir);
    if let Some(e) = earlier.iter().find(same) {
        return Err(if e == dir {
            format!("`tsCompat` lists `{dir}` twice")
        } else {
            format!(
                "`tsCompat` lists `{dir}` and `{e}`, which differ only in case (the same folder \
                 on case-insensitive file systems); keep one of them"
            )
        });
    }
    match earlier.iter().find(|e| within(dir, e) || within(e, dir)) {
        Some(e) if within(dir, e) => Err(format!(
            "`{dir}` is inside `{e}`, which `tsCompat` already lists"
        )),
        Some(e) => Err(format!(
            "`{dir}` contains `{e}`, which `tsCompat` already lists; keep one of them"
        )),
        None => Ok(()),
    }
}

/// Warnings for the `tsCompat` folders of manifest text `src` that are not folders under `root`
/// (the package root), each at the string that names it. Folders the reader rejects are left to
/// its errors, and a manifest it cannot read gives no warnings.
pub fn missing_ts_compat_dirs(file: FileId, src: &str, root: &Path) -> Diagnostics {
    let Ok((manifest, value)) = super::read_with_value(file, src) else {
        return vec![];
    };
    if manifest.ts_compat.is_empty() {
        return vec![];
    }
    let ValueKind::Object(fields) = &value.kind else {
        return vec![];
    };
    let Some((
        _,
        Value {
            kind: ValueKind::Array(elems),
            ..
        },
    )) = fields.iter().find(|(k, _)| k.name == "tsCompat")
    else {
        return vec![];
    };
    elems
        .iter()
        .filter_map(|elem| match &elem.kind {
            ValueKind::Str(dir) if !root.join(dir).is_dir() => {
                let what = if root.join(dir).exists() {
                    "is not a folder"
                } else {
                    "does not exist"
                };
                let mut d =
                    Diagnostic::error(format!("`tsCompat` folder `{dir}` {what}"), elem.span)
                        .with_note("`velt check --ts-compat` fails until it is created or removed");
                d.severity = Severity::Warning;
                Some(d)
            }
            _ => None,
        })
        .collect()
}
