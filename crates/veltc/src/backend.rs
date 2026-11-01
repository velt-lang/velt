//! Code generator selection: Cranelift (fast compiles; the debug default) or LLVM via clang
//! (fast code; the release default whenever clang is installed).

use std::time::Duration;

use velt_codegen_cl::{CacheStats, CodeCache};
use velt_vir::vir;

/// A code generation backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `velt_codegen_cl`.
    Cranelift,
    /// `velt_codegen_llvm` (needs clang at build time).
    Llvm,
}

impl Backend {
    /// Parse a `--backend` value.
    pub fn parse(name: &str) -> Result<Backend, String> {
        match name {
            "cranelift" | "cl" => Ok(Backend::Cranelift),
            "llvm" => Ok(Backend::Llvm),
            other => Err(format!(
                "unknown backend `{other}` (expected cranelift or llvm)"
            )),
        }
    }

    /// The backend to use: the explicit choice, else LLVM for release builds when clang is
    /// available, else Cranelift. The note explains a release build falling back to Cranelift.
    pub fn resolve(explicit: Option<Backend>, release: bool) -> (Backend, Option<String>) {
        match explicit {
            Some(b) => (b, None),
            None if !release => (Backend::Cranelift, None),
            None if velt_codegen_llvm::available() => (Backend::Llvm, None),
            None => {
                let why =
                    velt_codegen_llvm::rejected_clang().unwrap_or_else(|| "clang not found".into());
                let note = format!(
                    "note: {why}, so --release uses the Cranelift backend (install LLVM or set \
                     VELT_CLANG for faster code)"
                );
                (Backend::Cranelift, Some(note))
            }
        }
    }

    /// The x86 features this backend's code for `cpu` on `target` uses (the start-up check's
    /// mask, `driver/cpu_check.rs`): for a CPU name, Cranelift's preset of it or what clang
    /// enables for it.
    pub fn required_features(self, target: &str, cpu: Option<&str>) -> Result<u32, String> {
        match self {
            Backend::Cranelift => velt_codegen_cl::required_features(target, cpu),
            Backend::Llvm => velt_codegen_llvm::required_features(target, cpu),
        }
    }

    /// Generate the object files for `program`, appending the time of the backend's steps to
    /// `timings` where it reports them (LLVM: printing the IR, then clang). Cranelift makes one
    /// object; LLVM one per codegen unit (`units`, see `velt_codegen_llvm::emit_objects_timed`).
    /// Both go through the machine's build cache (`build_cache`); the stats say how much came
    /// from it (Cranelift: functions; LLVM: codegen units), `None` without a cache.
    pub fn emit_objects(
        self,
        program: &vir::Program,
        opts: &velt_codegen_cl::CodegenOptions,
        units: Option<usize>,
        timings: &mut Vec<(&'static str, Duration)>,
    ) -> Result<(Vec<Vec<u8>>, Option<CacheStats>), String> {
        let cache = crate::build_cache::cache();
        match self {
            Backend::Cranelift => {
                let cache = cache.map(crate::build_cache::Functions);
                let cache = cache.as_ref().map(|c| c as &dyn CodeCache);
                let (obj, stats) = velt_codegen_cl::emit_object_cached(program, opts, cache)?;
                Ok((vec![obj], cache.map(|_| stats)))
            }
            Backend::Llvm => {
                let cache = cache.map(crate::build_cache::Objects);
                let cache = cache.as_ref().map(|c| c as &dyn CodeCache);
                let (objs, stats) =
                    velt_codegen_llvm::emit_objects_cached(program, opts, units, timings, cache)?;
                Ok((objs, cache.map(|_| stats)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_resolve() {
        assert_eq!(Backend::parse("llvm"), Ok(Backend::Llvm));
        assert_eq!(Backend::parse("cranelift"), Ok(Backend::Cranelift));
        assert!(Backend::parse("gcc")
            .unwrap_err()
            .contains("unknown backend"));
        assert_eq!(
            Backend::resolve(Some(Backend::Llvm), false),
            (Backend::Llvm, None)
        );
        assert_eq!(Backend::resolve(None, false), (Backend::Cranelift, None));
        let (release, note) = Backend::resolve(None, true);
        if velt_codegen_llvm::available() {
            assert_eq!((release, note), (Backend::Llvm, None));
        } else {
            assert_eq!(release, Backend::Cranelift);
            assert!(note.unwrap().contains("clang"));
        }
    }
}
