//! Which std files a module may load (docs/internals/design/std-layout.md §2, §9).
//!
//! std's own test files (`std/<module>/<name>.test.vlt`, the white-box tests `velt test std/`
//! runs) are std code wherever the import that reaches them comes from: the generated harness of
//! `velt test` imports them relatively, and they import the internals they test.

use std::path::Path;

use super::locate::{Origin, Target};
use super::Loader;

impl Loader<'_, '_> {
    /// `target` with a std origin when it is one of std's test files (`file`, whose
    /// [`super::file_key`] is `key`) reached from outside std.
    pub(super) fn std_test_origin(&self, target: Target, file: &Path, key: &Path) -> Target {
        let (Some(std), Some(std_key)) = (&self.opts.std_root, &self.std_key) else {
            return target;
        };
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let test = crate::commands::test::discover::is_test_file(name);
        if matches!(target.origin, Origin::Std(_)) || !test || !key.starts_with(std_key) {
            return target;
        }
        Target {
            canonical: None,
            origin: Origin::Std(std.clone()),
            ..target
        }
    }
}
