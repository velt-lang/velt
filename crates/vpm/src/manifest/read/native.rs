//! Decoding the `native` object of `package.vlt`, with each check located at the value it
//! rejects.

use super::{Reader, Value, ValueKind};
use crate::manifest::{
    check_native_path, check_native_target, check_native_wasm, default_native_path, NativeConfig,
};

const NATIVE_KEYS: [&str; 3] = ["path", "targets", "wasm"];

impl Reader<'_> {
    /// `native: { path?: string; targets?: string[]; wasm?: boolean }`.
    pub(super) fn native(&mut self, value: &Value) -> Option<NativeConfig> {
        let mut config = NativeConfig {
            path: default_native_path(),
            targets: Vec::new(),
            wasm: false,
        };
        for (key, v) in self.object(value, "`native`")? {
            match key.name.as_str() {
                "path" => {
                    if let Some(s) = self.string(v, "path") {
                        self.check(check_native_path(s), v.span);
                        config.path = s.to_string();
                    }
                }
                "targets" => config.targets = self.native_targets(v),
                "wasm" => config.wasm = self.native_wasm(v),
                _ => self.unknown_key(key, &NATIVE_KEYS),
            }
        }
        Some(config)
    }

    fn native_targets(&mut self, value: &Value) -> Vec<String> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!("`targets` must be an array, not {}", value.kind.describe()),
                value.span,
            );
            return Vec::new();
        };
        let mut targets = Vec::new();
        for elem in elems {
            match &elem.kind {
                ValueKind::Str(target) => {
                    self.check(check_native_target(target), elem.span);
                    targets.push(target.clone());
                }
                other => self.error(
                    format!(
                        "`targets` entries must be strings, not {}",
                        other.describe()
                    ),
                    elem.span,
                ),
            }
        }
        targets
    }

    fn native_wasm(&mut self, value: &Value) -> bool {
        match value.kind {
            ValueKind::Bool(wasm) => {
                self.check(
                    check_native_wasm(wasm).map_err(|e| format!("`wasm: true` {e}")),
                    value.span,
                );
                wasm
            }
            ref other => {
                self.error(
                    format!("`wasm` must be a boolean, not {}", other.describe()),
                    value.span,
                );
                false
            }
        }
    }
}
