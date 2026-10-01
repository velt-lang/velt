//! Target triples: validation (the Cranelift backend's set plus the WebAssembly targets only this
//! backend supports) and the per-target function attributes that the Cranelift ISA settings
//! correspond to.
//!
//! The module carries no `target datalayout` line on purpose: clang fills in the layout of the
//! LLVM it ships, while a hard-coded string would be rejected ("data layout mismatch") as soon
//! as a newer LLVM changes the canonical layout for a target.

use crate::CodegenResult;

/// The operating systems / object formats the backend supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    /// COFF, MSVC ABI.
    Windows,
    /// Mach-O.
    Darwin,
    /// ELF.
    Linux,
    /// WebAssembly with WASI preview 1 (`wasm32-wasip1`): runs under wasmtime & co.
    Wasi,
    /// WebAssembly without an OS (`wasm32-unknown-unknown`): the browser, with JS glue.
    WasmBrowser,
}

/// A validated target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// The triple as given (or the host triple).
    pub triple: String,
    pub os: Os,
}

/// Validate `target` (empty / `native` / `host` = the host triple).
pub(crate) fn normalize(target: &str) -> CodegenResult<Target> {
    let target = target.trim();
    let triple = if target.is_empty() || target == "native" || target == "host" {
        crate::host_triple()
    } else {
        target.to_string()
    };
    if let Some(os) = wasm_os(&triple) {
        return Ok(Target { triple, os });
    }
    let mut parts = triple.split('-');
    let arch = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    let arch_ok = matches!(arch, "x86_64" | "aarch64" | "arm64");
    let os = if rest.iter().any(|p| p.starts_with("windows")) {
        Some(Os::Windows)
    } else if rest
        .iter()
        .any(|p| p.starts_with("darwin") || p.starts_with("macos"))
    {
        Some(Os::Darwin)
    } else if rest.iter().any(|p| p.starts_with("linux")) {
        Some(Os::Linux)
    } else {
        None
    };
    let msvc_ok = os != Some(Os::Windows) || !rest.iter().any(|p| p.starts_with("gnu"));
    match os {
        Some(os) if arch_ok && msvc_ok && !rest.is_empty() => Ok(Target { triple, os }),
        _ => Err(format!(
            "codegen: unsupported target `{triple}` (supported: x86_64/aarch64 × windows-msvc, \
             apple-darwin, unknown-linux-gnu; wasm32-wasip1, wasm32-unknown-unknown)"
        )),
    }
}

/// The WebAssembly targets: `wasm32-wasip1` (`wasm32-wasi` is its older name) and
/// `wasm32-unknown-unknown`.
fn wasm_os(triple: &str) -> Option<Os> {
    match triple {
        "wasm32-wasip1" | "wasm32-wasi" | "wasm32-unknown-wasip1" => Some(Os::Wasi),
        "wasm32-unknown-unknown" => Some(Os::WasmBrowser),
        _ => None,
    }
}

/// Whether `target` is one of the WebAssembly targets (for the driver: they need this backend
/// and the wasm linker).
pub fn is_wasm(target: &str) -> bool {
    wasm_os(target.trim()).is_some()
}

impl Target {
    /// String attributes shared by every defined function:
    /// - Apple's arm64/x86_64 ABIs expect frame pointers (clang's darwin default is non-leaf);
    /// - ELF/Mach-O get inline stack probes (no dependency on `__rust_probestack`); Windows
    ///   keeps LLVM's default `__chkstk` probing, which the MSVC CRT provides.
    /// - WebAssembly has no native stack to probe and no unwind tables.
    pub fn function_attributes(&self) -> Vec<&'static str> {
        if self.is_wasm() {
            return vec!["nounwind"];
        }
        let mut attrs = vec!["nounwind", "uwtable"];
        if self.os == Os::Darwin {
            attrs.push("\"frame-pointer\"=\"non-leaf\"");
        }
        if self.os != Os::Windows {
            attrs.push("\"probe-stack\"=\"inline-asm\"");
        }
        attrs
    }

    /// clang flag pinning the oldest macOS for a versionless Darwin triple. clang otherwise
    /// records the build machine's OS in the object, and linking it into an executable for an
    /// older minimum makes ld warn and the program require the newer OS. Same minimums as rustc
    /// and the Cranelift backend: 11.0 on arm64, 10.12 on x86_64.
    pub fn deployment_target_arg(&self) -> Option<&'static str> {
        let os_part = self.triple.split('-').nth(2).unwrap_or_default();
        if self.os != Os::Darwin || os_part.chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(if self.triple.starts_with("x86_64") {
            "-mmacosx-version-min=10.12"
        } else {
            "-mmacosx-version-min=11.0"
        })
    }

    /// Whether code for this target must be position independent (PIE executables).
    pub fn pic(&self) -> bool {
        !matches!(self.os, Os::Windows | Os::Wasi | Os::WasmBrowser)
    }

    /// Whether this is a WebAssembly target.
    pub fn is_wasm(&self) -> bool {
        matches!(self.os, Os::Wasi | Os::WasmBrowser)
    }

    /// Whether VIR's 64-bit `Ptr` slots in memory hold narrower machine pointers. VIR lays out
    /// memory with 8-byte pointers (vir.rs), but `wasm32` addresses are 32-bit: such targets
    /// keep the VIR layout and store each pointer zero-extended to 64 bits, loading it back
    /// with a truncation (see `function::place`). Registers and call signatures use the
    /// machine `ptr`, so the runtime's C ABI takes ordinary pointers.
    pub fn wide_pointer_slots(&self) -> bool {
        self.triple.starts_with("wasm32")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_triples() {
        for t in [
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
        ] {
            assert_eq!(normalize(t).unwrap().triple, t);
        }
        assert_eq!(normalize("").unwrap().triple, crate::host_triple());
        assert_eq!(normalize("x86_64-pc-windows-msvc").unwrap().os, Os::Windows);
    }

    #[test]
    fn wasm_triples() {
        let wasi = normalize("wasm32-wasip1").unwrap();
        assert_eq!(wasi.os, Os::Wasi);
        assert!(wasi.is_wasm() && wasi.wide_pointer_slots() && !wasi.pic());
        assert_eq!(normalize("wasm32-wasi").unwrap().os, Os::Wasi);
        let web = normalize("wasm32-unknown-unknown").unwrap();
        assert_eq!(web.os, Os::WasmBrowser);
        assert_eq!(web.function_attributes(), ["nounwind"]);
        assert!(is_wasm(" wasm32-wasip1") && !is_wasm("aarch64-apple-darwin"));
        assert!(!normalize("aarch64-apple-darwin")
            .unwrap()
            .wide_pointer_slots());
    }

    #[test]
    fn darwin_deployment_target() {
        let arg = |t: &str| normalize(t).unwrap().deployment_target_arg();
        assert_eq!(
            arg("aarch64-apple-darwin"),
            Some("-mmacosx-version-min=11.0")
        );
        assert_eq!(
            arg("x86_64-apple-darwin"),
            Some("-mmacosx-version-min=10.12")
        );
        assert_eq!(arg("aarch64-apple-macosx14.0"), None);
        assert_eq!(arg("aarch64-unknown-linux-gnu"), None);
    }

    #[test]
    fn unsupported_triples() {
        for t in [
            "riscv64-unknown-linux-gnu",
            "x86_64-pc-windows-gnu",
            "wasm32",
            "wasm64-unknown-unknown",
            "x86_64-unknown-freebsd",
        ] {
            assert!(
                normalize(t).unwrap_err().contains("unsupported target"),
                "{t}"
            );
        }
    }
}
