//! The tests of the compiler's std hook table (crates/velt_common/src/std_hooks.rs) that
//! `cargo xtask precheck` runs when a change can break them, so a renamed std function or a new
//! call of a runtime extern a hook replaces fails before the push, not in CI (#2197).

/// What a change needs of the hook tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookTests {
    /// `cargo test -p velt_sema --test std_hooks`: every entry resolves in std with its
    /// signature (a few seconds once built).
    pub signatures: bool,
    /// `cargo test -p velt_common --test hook_replaces`: the extern guard, which reads the
    /// compiler's sources, the runtime's and bench/stdport (well under a second), and the list
    /// of the runtime's exports, which only shrinks.
    pub guard: bool,
}

/// The table, the type language its signatures are written in (whose macros build every
/// `Sig`) and the std module table it names modules by.
const TABLE: &[&str] = &[
    "crates/velt_common/src/std_hooks.rs",
    "crates/velt_common/src/hook_sig.rs",
    "crates/velt_common/src/std_modules.rs",
];

/// The sources the guard reads: the compiler crates it scans, the runtime that defines the
/// externs, its own file and the benchmarks the table names.
const GUARDED: &[&str] = &[
    "crates/velt_sema/src/",
    "crates/velt_vir/src/",
    "crates/velt_opt/src/",
    "crates/velt_codegen_llvm/src/",
    "crates/velt_codegen_cl/src/",
    "crates/velt_rt/src/",
    "crates/velt_rt_wasm/src/",
    "crates/velt_common/tests/hook_replaces.rs",
    "crates/velt_common/tests/runtime_exports.txt",
    "bench/stdport/",
];

impl HookTests {
    /// Adds what a change to `path` (relative, `/`-separated) needs.
    pub fn add(&mut self, path: &str) {
        let table = TABLE.contains(&path);
        self.signatures |=
            table || path == "crates/velt_sema/tests/std_hooks.rs" || path.starts_with("std/");
        self.guard |= table || GUARDED.iter().any(|g| path.starts_with(g));
    }

    /// The `cargo test` arguments of each selected test, with its name and how to fix it.
    pub fn commands(&self) -> Vec<(&'static str, [&'static str; 5], &'static str)> {
        let mut out = vec![];
        if self.signatures {
            out.push((
                "std hook signatures (velt_sema std_hooks)",
                ["test", "-p", "velt_sema", "--test", "std_hooks"],
                "make velt_common's std_hooks.rs and std agree: each entry names a definition of \
                 std with the signature the table gives",
            ));
        }
        if self.guard {
            out.push((
                "replaced runtime externs (velt_common hook_replaces)",
                ["test", "-p", "velt_common", "--test", "hook_replaces"],
                "call the hook instead of the extern it replaces, remove the ALLOWED entries of a \
                 use you deleted, or the runtime_exports.txt lines of an export you deleted \
                 (crates/velt_common/tests/); a new runtime export is not allowed (#2196)",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(paths: &[&str]) -> HookTests {
        let mut t = HookTests::default();
        for p in paths {
            t.add(p);
        }
        t
    }

    #[test]
    fn std_and_the_table_run_the_signatures_and_the_compiler_the_guard() {
        let both = HookTests {
            signatures: true,
            guard: true,
        };
        assert_eq!(of(&["crates/velt_common/src/std_hooks.rs"]), both);
        assert_eq!(of(&["crates/velt_common/src/hook_sig.rs"]), both);
        let std = of(&["std/lang/hooks/number_text.velt"]);
        assert_eq!((std.signatures, std.guard), (true, false));
        for path in [
            "crates/velt_vir/src/lower/strbuf.rs",
            "crates/velt_rt/src/strbuf.rs",
            "bench/stdport/int_to_string.velt",
        ] {
            assert_eq!(
                of(&[path]),
                HookTests {
                    signatures: false,
                    guard: true
                },
                "{path}"
            );
        }
        for path in [
            "crates/velt_fmt/src/lib.rs",
            "docs/README.md",
            "crates/velt_vir/tests/a.rs",
        ] {
            assert_eq!(of(&[path]), HookTests::default(), "{path}");
        }
        assert_eq!(
            of(&["std/a.velt", "crates/velt_opt/src/lib.rs"])
                .commands()
                .len(),
            2
        );
    }

    /// CI selects the guard for the same sources (crates/xtask/src/plan.rs `SHARED_SOURCES`,
    /// read_by_tests.rs `bench/`), even for a comment-only change.
    #[test]
    fn ci_runs_the_guard_with_the_sources_it_reads() {
        use crate::graph::Graph;
        use crate::plan::Plan;
        let krate = |dir: &str, dep: &str| {
            let deps = match dep {
                "" => String::new(),
                d => format!("{d}.workspace = true\n"),
            };
            let toml = format!("[package]\nname = \"{dir}\"\n\n[dependencies]\n{deps}");
            (dir.to_string(), toml)
        };
        let g = Graph::from_manifests(&[
            krate("velt_common", ""),
            krate("velt_sema", "velt_common"),
            krate("velt_vir", "velt_sema"),
            krate("velt_opt", "velt_vir"),
            krate("velt_codegen_llvm", "velt_opt"),
            krate("velt_codegen_cl", "velt_vir"),
            krate("velt_rt", ""),
            krate("velt_lsp", "velt_sema"),
            krate("veltc", "velt_lsp"),
        ]);
        for path in [
            "crates/velt_sema/src/check.rs",
            "crates/velt_vir/src/lower/strbuf.rs",
            "crates/velt_opt/src/numrep/print.rs",
            "crates/velt_codegen_llvm/src/lib.rs",
            "crates/velt_codegen_cl/src/lib.rs",
            "crates/velt_rt/src/strbuf.rs",
            "bench/stdport/int_to_string.velt",
        ] {
            let paths = vec![path.to_string()];
            let p = Plan::for_paths(&g, &paths);
            assert!(
                !p.full && p.packages.contains("velt_common"),
                "{path}: {p:?}"
            );
            let comments = Plan::for_changes(&g, &paths, &paths.iter().cloned().collect());
            assert!(
                comments.packages.contains("velt_common"),
                "{path} (comments)"
            );
        }
        let lsp = Plan::for_paths(&g, &["crates/velt_lsp/src/hover.rs".to_string()]);
        assert!(!lsp.packages.contains("velt_common"));
    }
}
