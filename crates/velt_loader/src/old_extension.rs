//! Sources with the old `.vlt` extension, read for one more release (#2419), and the warning that
//! says who can rename them.

use std::path::Path;

use velt_common::{Diagnostic, Span};

use crate::locate::Origin;
use crate::Loader;

impl Loader<'_, '_> {
    /// A file with the old extension: named when it is the user's to rename, and once per package
    /// when it is a dependency's, which its user cannot fix.
    pub(crate) fn warn_old_extension(
        &mut self,
        file: velt_common::FileId,
        path: &Path,
        origin: &Origin,
    ) {
        let old = vpm::sources::OLD_EXTENSION;
        if path.extension().is_none_or(|e| e != old) {
            return;
        }
        let message = match origin {
            Origin::Std(_) => return,
            Origin::Root(_) => {
                format!("`.{old}` files are read for one more release: rename this one to `.velt`")
            }
            Origin::Package { name, .. } => {
                if !self.warned_old_extension.insert(name.clone()) {
                    return;
                }
                format!(
                    "package `{name}` has `.{old}` files, read for one more release: its author \
                     must rename them to `.velt`"
                )
            }
        };
        self.diags
            .push(Diagnostic::warning(message, Span::new(file, 0, 0)));
    }
}
