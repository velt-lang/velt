//! Decoding `hostModules` of `package.vlt` ([`crate::host_modules`]), with each check located at
//! the key or value it rejects.

use super::{Reader, Value};
use crate::host_modules::{check_file, check_name, HostModules};

impl Reader<'_> {
    /// `hostModules`: host module name → its file.
    pub(super) fn host_modules(&mut self, value: &Value) -> HostModules {
        let mut out = HostModules::new();
        for (key, v) in self.object(value, "`hostModules`").unwrap_or_default() {
            self.check(check_name(&key.name), key.span);
            if let Some(file) = self.string(v, &key.name) {
                self.check(check_file(file), v.span);
                out.insert(key.name.clone(), file.to_string());
            }
        }
        out
    }
}
