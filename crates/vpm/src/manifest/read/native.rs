//! Decoding the `native` object of `package.vlt`, with each check located at the value it
//! rejects.

use super::{Reader, Value, ValueKind};
use std::collections::BTreeMap;

use crate::manifest::{
    check_native_path, check_native_target, check_native_wasm, default_native_path, CLibraryConfig,
    NativeConfig,
};

/// The longest C library key and system library name: well inside every linker's limits. A name
/// of some thousands of characters makes `link.exe` lose track of its arguments.
const MAX_LIBRARY_NAME: usize = 64;

/// The error for a name longer than [`MAX_LIBRARY_NAME`], quoting enough of it to find it.
fn check_name_length(name: &str) -> Result<(), String> {
    if name.len() <= MAX_LIBRARY_NAME {
        return Ok(());
    }
    let start: String = name.chars().take(24).collect();
    Err(format!(
        "`{start}…` is too long for a library name: {} bytes, at most {MAX_LIBRARY_NAME}",
        name.len()
    ))
}

impl Reader<'_> {
    /// `native: { path?: string; targets?: string[]; wasm?: boolean; c?: … }`: the Rust crate
    /// (`None` when the object has only `c`) and the C libraries.
    pub(super) fn native(
        &mut self,
        value: &Value,
    ) -> (
        Option<NativeConfig>,
        Option<BTreeMap<String, CLibraryConfig>>,
    ) {
        let mut config = NativeConfig {
            path: default_native_path(),
            targets: Vec::new(),
            wasm: false,
        };
        let mut libraries = None;
        let Some(fields) = self.object(value, "`native`") else {
            return (None, libraries);
        };
        let only_c = !fields.is_empty() && fields.iter().all(|(k, _)| k.name == "c");
        for (key, v) in fields {
            match key.name.as_str() {
                "c" => libraries = Some(self.c_libraries(v)),
                "path" => {
                    if let Some(s) = self.string(v, "path") {
                        self.check(check_native_path(s), v.span);
                        config.path = s.to_string();
                    }
                }
                "targets" => config.targets = self.native_targets(v),
                "wasm" => config.wasm = self.native_wasm(v),
                _ => self.unknown_key(key, crate::manifest::schema::NATIVE),
            }
        }
        ((!only_c).then_some(config), libraries)
    }

    /// `c: { <library>: { system?: Record<string, string>, headers?: string[], unchecked?:
    /// boolean } }`; the entry `c`, the platform's C library, only has `headers`.
    fn c_libraries(&mut self, value: &Value) -> BTreeMap<String, CLibraryConfig> {
        let mut out = BTreeMap::new();
        let Some(entries) = self.object(value, "`native.c`") else {
            return out;
        };
        for (name, v) in entries {
            if let Err(long) = check_name_length(&name.name) {
                self.error(long, name.span);
                continue;
            }
            if name.name.is_empty()
                || !name
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                self.error(
                    format!(
                        "`{}` is not a C library name: letters, digits, `_` and `-`",
                        name.name
                    ),
                    name.span,
                );
                continue;
            }
            let mut lib = CLibraryConfig::default();
            if let Some(fields) = self.object(v, "a C library") {
                for (key, f) in fields {
                    match key.name.as_str() {
                        "system" if name.name == "c" => self.error(
                            "`c` is the platform's C library, which every target links: it has no `system`",
                            key.span,
                        ),
                        "headers" => {
                            let ValueKind::Array(elems) = &f.kind else {
                                self.error(
                                    format!("`headers` must be an array, not {}", f.kind.describe()),
                                    f.span,
                                );
                                continue;
                            };
                            for elem in elems {
                                if let Some(text) = self.string(elem, "a header") {
                                    self.check(check_header(text), elem.span);
                                    lib.headers.push(text.to_string());
                                }
                            }
                        }
                        "unchecked" => match f.kind {
                            ValueKind::Bool(b) => lib.unchecked = b,
                            ref other => self.error(
                                format!("`unchecked` must be a boolean, not {}", other.describe()),
                                f.span,
                            ),
                        },
                        "system" => {
                            if let Some(conds) = self.object(f, "`system`") {
                                for (cond, s) in conds {
                                    let condition = check_target_condition(&cond.name);
                                    let known = condition.is_ok();
                                    self.check(condition, cond.span);
                                    if let Some(text) = self.string(s, "a system library") {
                                        let checked = check_system_library(text)
                                            .and_then(|()| check_framework(&cond.name, text));
                                        // Only what passes can reach `link_args`, even through
                                        // a caller that ignores the manifest's errors.
                                        if known && checked.is_ok() {
                                            lib.system.push((cond.name.clone(), text.to_string()));
                                        }
                                        self.check(checked, s.span);
                                    }
                                }
                            }
                        }
                        _ => self.unknown_key(key, crate::manifest::schema::C_LIBRARY),
                    }
                }
            }
            out.insert(name.name.clone(), lib);
        }
        out
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

    /// An array of strings (`allowNative`), each in the order written.
    pub(super) fn string_list(&mut self, value: &Value, what: &str) -> Vec<String> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!("`{what}` must be an array, not {}", value.kind.describe()),
                value.span,
            );
            return Vec::new();
        };
        let mut out = Vec::new();
        for elem in elems {
            match &elem.kind {
                ValueKind::Str(s) => out.push(s.clone()),
                other => self.error(
                    format!("`{what}` entries must be strings, not {}", other.describe()),
                    elem.span,
                ),
            }
        }
        out
    }
}

/// A `system` library: `name` (linked as `-lname`, `name.lib` on Windows) or `framework:Name`
/// (a macOS framework). Letters, digits, `_`, `-`, `.` and `+` only, not starting with `-`, so
/// a manifest names a library and never passes the linker a flag.
pub(crate) fn check_system_library(text: &str) -> Result<(), String> {
    let name = text.strip_prefix("framework:").unwrap_or(text);
    check_name_length(name)?;
    let ok = !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'));
    if !ok {
        return Err(format!(
            "`{text}` is not a system library: a library name such as `sqlite3` (linked as \
             `-lsqlite3`), or `framework:Name` for a macOS framework"
        ));
    }
    match library_of_file(name) {
        Some(library) => Err(format!(
            "`{text}` is a library's file: name the library, not its file: `{library}`"
        )),
        None => Ok(()),
    }
}

/// The library whose file `name` (checked to be ASCII) names: `foo` of `foo.lib`, `foo.dll`,
/// `libfoo.a`, `libfoo.so`, `libfoo.so.1` and `libfoo.dylib`; `None` for anything else, such as
/// `python3.11`. File names differ by case on Windows only, so any case counts.
fn library_of_file(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    let versioned = lower.find(".so.").filter(|at| {
        lower[at + 4..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.')
    });
    let unix = [".a", ".so", ".dylib"];
    let windows = [".lib", ".dll"];
    let ends = |suffixes: &[&str]| {
        suffixes
            .iter()
            .find(|s| lower.ends_with(**s))
            .map(|s| name.len() - s.len())
    };
    if let Some(at) = ends(&windows) {
        return Some(&name[..at]);
    }
    let stem = &name[..versioned.or_else(|| ends(&unix))?];
    Some(
        stem.strip_prefix("lib")
            .filter(|s| !s.is_empty())
            .unwrap_or(stem),
    )
}

/// A framework exists only on macOS, and another target's linker would take `-framework`'s name
/// for an input file, so `framework:Name` is only for the `darwin` condition.
fn check_framework(condition: &str, text: &str) -> Result<(), String> {
    if text.starts_with("framework:") && condition != "darwin" {
        return Err(format!(
            "`{text}` under `{condition}`: a framework is macOS-only: put it under `darwin`"
        ));
    }
    Ok(())
}

/// A `system` condition: one a native target's build has (a library is linked, so WebAssembly's
/// and host packages' conditions never apply).
fn check_target_condition(condition: &str) -> Result<(), String> {
    const TARGET: &[&str] = &["native", "darwin", "linux", "win32", "default"];
    if TARGET.contains(&condition) {
        return Ok(());
    }
    Err(format!(
        "`{condition}` is not a condition of a native target: a C library's are {}",
        TARGET
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `Ok` for a header entry: `<name.h>`, or a `/`-separated path inside the package.
fn check_header(text: &str) -> Result<(), String> {
    if let Some(inner) = text.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        let ok = !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-./".contains(c))
            && !inner.split('/').any(|p| p == "..");
        return match ok {
            true => Ok(()),
            false => Err(format!("`{text}` is not a header name")),
        };
    }
    let inside = !text.is_empty()
        && !text.starts_with('/')
        && !text.contains('\\')
        && !text.contains(':')
        && !text.split('/').any(|p| p == ".." || p.is_empty());
    match inside {
        true => Ok(()),
        false => Err(format!(
            "`{text}` is not a header of the package: a `/`-separated path inside it (`native/lib.h`), or `<name.h>` for a C library header"
        )),
    }
}
