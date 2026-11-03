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
    /// compiler's sources, the runtime's and bench/stdport (well under a second).
    pub guard: bool,
}

/// The table and the std module table it names modules by.
const TABLE: &[&str] = &[
    "crates/velt_common/src/std_hooks.rs",
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
    "crates/velt_common/tests/hook_replaces.rs",
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
                "call the hook instead of the extern it replaces, or remove the ALLOWED entries \
                 of a use you deleted (crates/velt_common/tests/hook_replaces.rs)",
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
        let std = of(&["std/lang/hooks/number_text.vlt"]);
        assert_eq!((std.signatures, std.guard), (true, false));
        for path in [
            "crates/velt_vir/src/lower/strbuf.rs",
            "crates/velt_rt/src/strbuf.rs",
            "bench/stdport/int_to_string.vlt",
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
            of(&["std/a.vlt", "crates/velt_opt/src/lib.rs"])
                .commands()
                .len(),
            2
        );
    }
}
