//! Which builds allocate with mimalloc directly, through std's hooks
//! (`velt_vir::LowerOptions::heap_hooks`, std/lang/hooks/heap.velt, #2196), rather than through
//! the runtime's allocator (`velt_rt_alloc`/`velt_rt_free`).
//!
//! Release builds for native targets that link a release runtime with mimalloc in it. Not:
//! - under the JIT (`velt dev`) nor on WebAssembly, whose runtimes keep their own;
//! - with a debug runtime (what a `velt` built by cargo links, or `$VELT_RT_LIB` names): its
//!   allocator checks and counts every block when the program runs with `VELT_RT_DEBUG_ALLOC=1`
//!   or `VELT_RC_STATS=1` (`debug_alloc.rs`), wrapping Rust's global allocator, so a block from
//!   mimalloc directly would reach it without its header, and its counts would leave it out. The
//!   variables are read when the program runs, so only the runtime tells;
//! - with a runtime built without the `mimalloc` feature, which has no `mi_*` functions to link;
//! - when the runtime's symbol index can't be read (no runtime found, not an archive);
//! - when the build itself sets `VELT_RT_DEBUG_ALLOC=1` or `VELT_RC_STATS=1`.
//!
//! `VELT_HEAP_HOOKS=0` turns them off; `VELT_HEAP_HOOKS=1` turns them on with a debug runtime
//! (that has mimalloc), for tests of the hooks themselves, whose programs then must not run with
//! the debug runtime's checks or counts.

use std::ffi::OsString;

use velt_link::RuntimeProfile;

/// Whether a build lowers with std's heap hooks; `jit` for the JIT, `target` the target triple.
pub(super) fn heap_hooks(release: bool, jit: bool, target: &str) -> bool {
    if !release || jit || velt_codegen_llvm::is_wasm(target) {
        return false;
    }
    let env = |name: &str| std::env::var_os(name);
    // Read only for a release build that may use the hooks: the symbol index of a debug runtime
    // is a few MB.
    let runtime = || {
        velt_link::find_runtime_lib(target)
            .ok()
            .and_then(|lib| velt_link::runtime_lib_profile(&lib))
    };
    decide(env, runtime)
}

/// [`heap_hooks`] for a native release build: `env` reads a variable, `runtime` the profile of
/// the runtime library the build links.
fn decide(
    env: impl Fn(&str) -> Option<OsString>,
    runtime: impl FnOnce() -> Option<RuntimeProfile>,
) -> bool {
    let is = |name: &str, value: &str| env(name).is_some_and(|v| v == value);
    if is("VELT_HEAP_HOOKS", "0") || is("VELT_RT_DEBUG_ALLOC", "1") || is("VELT_RC_STATS", "1") {
        return false;
    }
    match runtime() {
        Some(p) => p.mimalloc && (!p.debug || is("VELT_HEAP_HOOKS", "1")),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(vars: &[(&str, &str)], runtime: Option<(bool, bool)>) -> bool {
        let env = |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        };
        let runtime = runtime.map(|(debug, mimalloc)| RuntimeProfile { debug, mimalloc });
        decide(env, || runtime)
    }

    #[test]
    fn a_release_runtime_with_mimalloc_only() {
        let release = Some((false, true));
        assert!(with(&[], release));
        // A debug runtime checks or counts blocks if the program runs with the variables set.
        assert!(!with(&[], Some((true, true))));
        // No `mi_*` functions to link (built without the `mimalloc` feature), release or debug.
        assert!(!with(&[], Some((false, false))));
        assert!(!with(&[("VELT_HEAP_HOOKS", "1")], Some((true, false))));
        // A runtime that can't be read.
        assert!(!with(&[], None));
        assert!(!with(&[("VELT_HEAP_HOOKS", "1")], None));
    }

    #[test]
    fn the_variables_turn_them_off_or_on_with_a_debug_runtime() {
        let release = Some((false, true));
        assert!(!with(&[("VELT_HEAP_HOOKS", "0")], release));
        assert!(!with(&[("VELT_RC_STATS", "1")], release));
        assert!(!with(&[("VELT_RT_DEBUG_ALLOC", "1")], release));
        assert!(with(&[("VELT_RC_STATS", "0")], release));
        let debug = Some((true, true));
        assert!(with(&[("VELT_HEAP_HOOKS", "1")], debug));
        let counted = [("VELT_HEAP_HOOKS", "1"), ("VELT_RC_STATS", "1")];
        assert!(!with(&counted, debug));
    }
}
