//! Which files the corpus tests go through, and which goldens the debug-variable test checks.

use super::*;
use crate::sweep::MAX_SWEPT_FILES;

#[test]
fn the_parser_and_its_dependencies_sweep_every_golden_in_full() {
    for path in [
        "crates/velt_syntax/src/parser/expr.rs",
        "crates/velt_syntax/tests/common/mod.rs",
        "crates/velt_syntax/Cargo.toml",
        "crates/velt_common/src/span.rs",
    ] {
        let p = plan(&[path, "tests/golden/lang/foo.velt"]);
        assert_eq!(p.sweep, Sweep::Full, "{path}");
        assert_eq!(p.sweep_env()[0], ("VELT_ROBUSTNESS", "full".to_string()));
    }
}

#[test]
fn other_changes_sweep_only_the_changed_goldens() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "std/string.velt",
        "docs/internals/README.md",
        ".github/workflows/ci.yml",
    ] {
        assert_eq!(plan(&[path]).sweep, Sweep::Changed(set(&[])), "{path}");
        assert_eq!(
            plan(&[path]).sweep_env()[..2],
            [
                ("VELT_ROBUSTNESS", "changed".to_string()),
                ("VELT_ROBUSTNESS_FILES", String::new())
            ],
            "{path}"
        );
    }
    let p = plan(&[
        "tests/golden/lang/foo.velt",
        "tests/golden/lang/foo.out",
        "tests/golden/m1/errors/bar.velt",
        "examples/hello.velt",
        "crates/velt_sema/src/check.rs",
    ]);
    assert_eq!(
        p.sweep,
        Sweep::Changed(set(&[
            "tests/golden/lang/foo.velt",
            "tests/golden/m1/errors/bar.velt"
        ]))
    );
    assert_eq!(
        p.sweep_env()[1],
        (
            "VELT_ROBUSTNESS_FILES",
            "tests/golden/lang/foo.velt,tests/golden/m1/errors/bar.velt".to_string()
        )
    );
    // A change that selects everything still says which goldens it touched.
    let p = plan(&[".github/workflows/ci.yml", "tests/golden/lang/foo.velt"]);
    assert!(p.full);
    assert_eq!(
        p.sweep,
        Sweep::Changed(set(&["tests/golden/lang/foo.velt"]))
    );
}

#[test]
fn the_formatter_corpus_is_the_changed_files_unless_the_formatter_changed() {
    let p = plan(&[
        "std/string.velt",
        "tests/golden/lang/foo.velt",
        "tests/golden/lang/foo.out",
        "fuzz/seeds/a.velt",
        "examples/hello.velt",
        "crates/velt_sema/src/check.rs",
    ]);
    assert_eq!(
        p.fmt_corpus,
        Sweep::Changed(set(&[
            "fuzz/seeds/a.velt",
            "std/string.velt",
            "tests/golden/lang/foo.velt"
        ]))
    );
    let env = p.sweep_env();
    assert!(env.contains(&("VELT_FMT_CORPUS", "changed".to_string())));
    assert!(env.contains(&(
        "VELT_FMT_CORPUS_FILES",
        "fuzz/seeds/a.velt,std/string.velt,tests/golden/lang/foo.velt".to_string()
    )));
    // The formatter, what it depends on, and how it is built: the whole corpus. A change of the
    // parser is one, a change of the formatter leaves the parser's goldens alone.
    for path in [
        "crates/velt_syntax/src/lexer.rs",
        "crates/velt_common/src/span.rs",
        "rust-toolchain.toml",
        "Cargo.toml",
    ] {
        let p = plan(&[path, "std/string.velt"]);
        assert_eq!(p.fmt_corpus, Sweep::Full, "{path}");
        assert!(
            !p.sweep_env().iter().any(|(v, _)| v.contains("FMT")),
            "{path}"
        );
    }
    let p = plan(&["crates/velt_fmt/src/lib.rs"]);
    assert_eq!(p.fmt_corpus, Sweep::Full);
    assert_eq!(p.sweep, Sweep::Changed(set(&[])));
}

#[test]
fn comment_only_parser_changes_and_many_goldens() {
    let paths = vec!["crates/velt_syntax/src/lexer/mod.rs".to_string()];
    let p = Plan::for_changes(
        &graph(),
        &paths,
        &set(&["crates/velt_syntax/src/lexer/mod.rs"]),
    );
    assert_eq!(p.sweep, Sweep::Changed(set(&[])));
    let many: Vec<String> = (0..=MAX_SWEPT_FILES)
        .map(|i| format!("tests/golden/lang/g{i}.velt"))
        .collect();
    assert_eq!(Plan::for_paths(&graph(), &many).sweep, Sweep::Full);
    assert!(matches!(
        Plan::for_paths(&graph(), &many[1..]).sweep,
        Sweep::Changed(files) if files.len() == MAX_SWEPT_FILES
    ));
}

/// A comment is what the formatter must keep and what the parser's cuts go through.
#[test]
fn a_comment_only_change_of_a_program_is_swept() {
    let golden = "tests/golden/lang/foo.velt";
    let std = "std/string.velt";
    let p = Plan::for_changes(
        &graph(),
        &[golden.to_string(), std.to_string()],
        &set(&[golden, std]),
    );
    assert_eq!(p.sweep, Sweep::Changed(set(&[golden])));
    assert_eq!(p.fmt_corpus, Sweep::Changed(set(&[golden, std])));
}

/// Neither test reads a `.ts` or `.tsx` file: `golden_files` and `corpus` keep `.velt` only.
#[test]
fn only_velt_files_are_swept() {
    let p = plan(&["tests/golden/lang/foo.ts", "tests/golden/lang/bar.tsx"]);
    assert_eq!(p.sweep, Sweep::Changed(set(&[])));
    assert_eq!(p.fmt_corpus, Sweep::Changed(set(&[])));
}

#[test]
fn the_toolchain_and_the_root_manifest_sweep_in_full() {
    for path in ["rust-toolchain.toml", "Cargo.toml"] {
        assert_eq!(plan(&[path]).sweep, Sweep::Full, "{path}");
    }
    // A crate's manifest other than the parser's changes nothing about it.
    assert_eq!(
        plan(&["crates/velt_sema/Cargo.toml"]).sweep,
        Sweep::Changed(set(&[]))
    );
}

#[test]
fn the_dependencies_sweep_all_the_files_at_the_tests_default() {
    for path in ["Cargo.lock", ".cargo/config.toml"] {
        let p = plan(&[path, "tests/golden/lang/foo.velt"]);
        assert_eq!(p.sweep, Sweep::Default, "{path}");
        assert_eq!(p.fmt_corpus, Sweep::Default, "{path}");
        assert!(p.sweep_env().is_empty(), "{path}");
    }
}

#[test]
fn unknown_changes_sweep_in_full_but_explicit_full_leaves_it_to_the_environment() {
    let unknown = Plan::unknown_changes("no base revision");
    assert!(unknown.full);
    assert_eq!(unknown.sweep, Sweep::Full);
    assert_eq!(
        unknown.sweep_env(),
        [
            ("VELT_ROBUSTNESS", "full".to_string()),
            ("VELT_DEBUG_VARS", "all".to_string())
        ]
    );
    let full = Plan::everything("--full");
    assert_eq!(full.sweep, Sweep::Default);
    assert_eq!(full.fmt_corpus, Sweep::Default);
    assert!(full.sweep_env().is_empty());
}

#[test]
fn the_code_describing_variables_checks_every_golden_with_it() {
    for path in [
        "crates/velt_vir/src/lower/debug_vars.rs",
        "crates/velt_vir/src/verify/debug.rs",
        "crates/velt_vir/src/vir/debug.rs",
        "crates/veltc/tests/debug_vars.rs",
    ] {
        let p = plan(&[path]);
        assert!(p.debug_vars_all, "{path}");
        assert!(p
            .sweep_env()
            .contains(&("VELT_DEBUG_VARS", "all".to_string())));
        // A comment changes nothing about it.
        let only_comments = Plan::for_changes(&graph(), &[path.to_string()], &set(&[path]));
        assert!(!only_comments.debug_vars_all, "{path}");
    }
    // Other code keeps the sample, with a change that selects everything too.
    for path in [
        "crates/velt_vir/src/lower/mod.rs",
        "std/string.velt",
        "Cargo.lock",
    ] {
        assert!(!plan(&[path]).debug_vars_all, "{path}");
    }
    // Together with one that selects everything it still counts.
    assert!(plan(&["Cargo.lock", "crates/velt_vir/src/vir/debug.rs"]).debug_vars_all);
    // `--full` leaves it to the environment (the nightly's), and unknown changes check them all.
    assert!(!Plan::everything("--full").debug_vars_all);
    assert!(Plan::unknown_changes("no base").debug_vars_all);
}
