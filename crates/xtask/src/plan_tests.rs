use super::*;

fn graph() -> Graph {
    let krate = |dir: &str, deps: &[&str]| {
        let deps: String = deps
            .iter()
            .map(|d| format!("{d}.workspace = true\n"))
            .collect();
        (
            dir.to_string(),
            format!("[package]\nname = \"{dir}\"\n\n[dependencies]\n{deps}"),
        )
    };
    Graph::from_manifests(&[
        krate("velt_common", &[]),
        krate("velt_syntax", &["velt_common"]),
        krate("velt_sema", &["velt_syntax"]),
        krate("velt_fmt", &["velt_syntax"]),
        krate("velt_lsp", &["velt_sema", "velt_fmt"]),
        krate("vpm", &["velt_fmt"]),
        krate("velt_rt", &[]),
        krate("velt_rt_host", &[]),
        krate("velt_rt_shared", &[]),
        krate("velt_rt_wasm", &[]),
        krate("veltc", &["velt_sema", "velt_lsp", "vpm"]),
        krate("xtask", &[]),
    ])
}

fn plan(paths: &[&str]) -> Plan {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    Plan::for_paths(&graph(), &paths)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn build_configuration_ci_and_unknown_paths_select_everything() {
    for path in [
        "Cargo.lock",
        "Cargo.toml",
        "crates/velt_sema/Cargo.toml",
        ".github/workflows/ci.yml",
        "scripts/check-all.sh",
        "crates/xtask/src/plan.rs",
        "crates/velt_gone/src/lib.rs",
        "something/new.txt",
    ] {
        assert!(plan(&[path]).full, "{path}");
    }
}

#[test]
fn prose_and_editor_files_select_nothing() {
    let p = plan(&[
        "CONTRIBUTING.md",
        "editors/vscode/package.json",
        "LICENSE-MIT",
    ]);
    assert!(!p.full);
    assert!(!p.needs_build());
    assert_eq!(p.filterset(), None);
}

#[test]
fn a_pipeline_crate_runs_its_dependents_and_every_end_to_end_test() {
    let p = plan(&["crates/velt_sema/src/check.rs"]);
    assert!(!p.full && p.rust);
    assert_eq!(p.packages, set(&["velt_lsp", "velt_sema"]));
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
    assert_eq!(
        p.filterset().unwrap(),
        "(package(velt_lsp) | package(velt_sema) | package(veltc)) - binary_id(veltc::golden)"
    );
}

#[test]
fn the_runtime_has_no_dependents_but_runs_every_end_to_end_test() {
    let p = plan(&["crates/velt_rt/src/http.rs"]);
    // The crates compiling the runtime's sources run their tests too.
    assert_eq!(
        p.packages,
        set(&["velt_rt", "velt_rt_host", "velt_rt_shared", "velt_rt_wasm"])
    );
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
}

#[test]
fn a_tooling_crate_runs_veltc_unit_tests_and_its_binaries_only() {
    let p = plan(&["crates/velt_lsp/src/hover.rs"]);
    assert_eq!(p.packages, set(&["velt_lsp"]));
    assert_eq!(p.veltc, Veltc::Some(set(&[])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(!p.vlt_fmt);
    assert_eq!(
        p.filterset().unwrap(),
        "(package(velt_lsp) | (package(veltc) & (kind(lib) | kind(bin) | binary(standards)))) \
         - binary_id(veltc::golden)"
    );
    let wasm = plan(&["crates/velt_rt_wasm/src/lib.rs"]);
    assert_eq!(
        wasm.veltc,
        Veltc::Some(set(&["playground", "wasm_goldens"]))
    );
}

#[test]
fn the_formatter_reaches_its_dependents_and_the_vlt_format_check() {
    let p = plan(&["crates/velt_fmt/src/jsx.rs"]);
    assert_eq!(p.packages, set(&["velt_fmt", "velt_lsp", "vpm"]));
    assert!(p.vlt_fmt);
    // vpm is a dependent and resolves every program's imports.
    assert_eq!(p.goldens, Goldens::All);
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    assert!(binaries.contains("templates") && binaries.contains("native_packages"));
}

#[test]
fn a_changed_golden_runs_that_golden_and_the_tests_reading_goldens() {
    let p = plan(&[
        "tests/golden/lang/foo.vlt",
        "tests/golden/lang/foo.out",
        "tests/golden/m2/errors/bad.err",
    ]);
    assert!(!p.rust);
    assert_eq!(
        p.golden_env().unwrap(),
        "tests/golden/lang/foo.,tests/golden/m2/errors/bad."
    );
    assert!(p.packages.contains("velt_sema"));
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    assert!(binaries.contains("wasm_goldens"));
}

#[test]
fn shared_golden_files_select_their_directory() {
    let filter = |path: &str| golden_filter(path).unwrap();
    assert_eq!(
        filter("tests/golden/lang/_helper.vlt"),
        "tests/golden/lang/"
    );
    assert_eq!(
        filter("tests/golden/lang/_mod/index.vlt"),
        "tests/golden/lang/"
    );
    assert_eq!(
        filter("tests/golden/lang/errors/_p/x.vlt"),
        "tests/golden/lang/"
    );
    assert_eq!(filter("tests/golden/bugs/.pending"), "tests/golden/bugs/");
    assert_eq!(
        filter("tests/golden/lang/jsx_package/pages/a.vlt"),
        "tests/golden/lang/"
    );
    assert_eq!(
        filter("tests/golden/lang/errors/bad.err"),
        "tests/golden/lang/errors/bad."
    );
    assert_eq!(
        filter("tests/golden/std/http.code"),
        "tests/golden/std/http."
    );
    assert_eq!(filter("examples/bank.out"), "examples/bank.");
    assert_eq!(filter("examples/apps/todo/main.vlt"), "examples/apps/");
}

#[test]
fn the_standard_library_runs_every_test_but_no_clippy() {
    let p = plan(&["std/json.vlt"]);
    assert!(!p.full && !p.rust);
    assert!(p.packages.contains("velt_rt") && p.packages.contains("velt_fmt"));
    assert!(!p.packages.contains("veltc"));
    assert_eq!(p.filterset().unwrap(), "all() - binary_id(veltc::golden)");
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
    assert!(p.vlt_fmt);
}

#[test]
fn docs_run_the_documentation_tests() {
    let p = plan(&["docs/reference/types.md"]);
    assert_eq!(p.packages, set(&["velt_doc"]));
    assert_eq!(p.veltc, Veltc::Some(set(&["docs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.needs_build());
}

#[test]
fn rules_combine() {
    let p = plan(&[
        "docs/std/fs.md",
        "crates/velt_lsp/src/a.rs",
        "tests/golden/std/fs.vlt",
    ]);
    assert!(p.packages.contains("velt_doc") && p.packages.contains("velt_lsp"));
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    assert!(binaries.contains("docs") && binaries.contains("wasm_goldens"));
    assert_eq!(p.golden_env().unwrap(), "tests/golden/std/fs.");
    // A pipeline crate on top makes the end-to-end tests run in full.
    let more = plan(&["docs/std/fs.md", "crates/velt_syntax/src/lexer.rs"]);
    assert_eq!(more.veltc, Veltc::All);
    assert_eq!(more.goldens, Goldens::All);
}

/// The tables name real crates and `veltc` test binaries: a rename must update them, or the
/// tests it names would silently stop being selected.
#[test]
fn the_rules_name_what_exists_in_this_repository() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let graph = Graph::load(&root).unwrap();
    let crates: Vec<&str> = PIPELINE
        .iter()
        .chain(TOOLING.iter().map(|(c, _)| c))
        .chain(GOLDENS_TOO)
        .chain(
            SHARED_SOURCES
                .iter()
                .flat_map(|(c, readers)| std::iter::once(c).chain(readers.iter())),
        )
        .chain(
            READ_BY_TESTS
                .iter()
                .flat_map(|(_, packages, _)| packages.iter()),
        )
        .copied()
        .collect();
    for name in crates {
        assert!(
            graph.deps.contains_key(name),
            "no crate `{name}` in crates/"
        );
    }
    let binaries = TOOLING
        .iter()
        .flat_map(|(_, b)| b.iter())
        .chain(READ_BY_TESTS.iter().flat_map(|(_, _, b)| b.iter()))
        .chain(&["standards", "golden"]);
    for name in binaries {
        let file = root.join(format!("crates/veltc/tests/{name}.rs"));
        assert!(file.exists(), "no test binary {}", file.display());
    }
}

#[test]
fn os_specific_changes_check_windows_and_macos_in_the_queue() {
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        // Terminal echo, signals and file permissions differ by OS (#315).
        "crates/veltc/src/commands/registry.rs",
        "crates/vpm/src/credentials.rs",
        "crates/vpm/src/json_file.rs",
        "std/fs.vlt",
        "tests/golden/lang/foo.out",
        "Cargo.lock",
        ".github/workflows/ci.yml",
    ] {
        assert!(plan(&[path]).other_os.is_some(), "{path}");
    }
    // A crate the rules don't know counts as OS-specific.
    let unknown = Graph::from_manifests(&[(
        "velt_new".to_string(),
        "[package]\nname = \"velt_new\"\n".to_string(),
    )]);
    let paths = vec!["crates/velt_new/src/lib.rs".to_string()];
    assert!(Plan::for_paths(&unknown, &paths).other_os.is_some());
}

#[test]
fn portable_changes_check_linux_only() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "crates/velt_lsp/src/hover.rs",
        "crates/velt_fmt/src/jsx.rs",
        "docs/reference/types.md",
        "CONTRIBUTING.md",
    ] {
        assert_eq!(plan(&[path]).other_os, None, "{path}");
    }
}

#[test]
fn the_portable_crates_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let graph = Graph::load(&root).unwrap();
    for name in crate::os::PORTABLE_CRATES {
        assert!(
            graph.deps.contains_key(*name),
            "no crate `{name}` in crates/"
        );
    }
}

#[test]
fn the_differential_tester_checks_only_itself() {
    let p = plan(&[
        "tests/difftest/src/gen/expr_scalar.rs",
        "tests/difftest/corpus/knapsack.vlt",
    ]);
    assert!(!p.full);
    assert!(p.difftest);
    assert!(!p.rust, "no workspace clippy");
    assert!(!p.needs_build(), "no workspace build");
    assert_eq!(p.filterset(), None);
    assert_eq!(p.goldens, Goldens::None);
    assert_eq!(p.other_os, None);
    // Its manifest too: the crate is outside the workspace, so it can't change other crates.
    assert!(plan(&["tests/difftest/Cargo.toml"]).difftest);
    // Workspace changes leave it alone; everything includes it.
    assert!(!plan(&["crates/velt_syntax/src/lexer.rs"]).difftest);
    assert!(plan(&["Cargo.lock"]).difftest);
}

/// The differential tester's manifest exists where the rule says.
#[test]
fn the_differential_tester_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(root.join(DIFFTEST).join("Cargo.toml").is_file());
}

#[test]
fn comment_only_changes_check_the_crate_alone_on_linux() {
    let paths = vec![
        "crates/velt_rt/src/postgres/batch/abi.rs".to_string(),
        "crates/velt_sema/src/check.rs".to_string(),
        "docs/reference/types.md".to_string(),
    ];
    let p = Plan::for_changes(&graph(), &paths, &paths[..2].iter().cloned().collect());
    assert!(!p.full && p.rust);
    assert_eq!(
        p.packages,
        set(&[
            "velt_doc",
            "velt_rt",
            "velt_rt_host",
            "velt_rt_shared",
            "velt_rt_wasm",
            "velt_sema"
        ])
    );
    assert_eq!(p.veltc, Veltc::Some(set(&["docs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(!p.vlt_fmt);
    assert_eq!(p.other_os, None);
    assert!(
        p.filterset().unwrap().contains("binary(standards)"),
        "{:?}",
        p.filterset()
    );
}

#[test]
fn a_code_change_in_the_same_crate_outweighs_comment_only_files() {
    let paths = vec![
        "crates/velt_rt/src/a.rs".to_string(),
        "crates/velt_rt/src/b.rs".to_string(),
    ];
    let p = Plan::for_changes(&graph(), &paths, &set(&["crates/velt_rt/src/a.rs"]));
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
    assert!(p.other_os.is_some());
    // Comments in the tooling select everything all the same.
    let tooling = vec!["crates/xtask/src/plan.rs".to_string()];
    assert!(Plan::for_changes(&graph(), &tooling, &tooling.iter().cloned().collect()).full);
}

/// A comment edit in the runtime can't change its ABI symbol table: velt_rt's build script
/// scans the sources with comments removed, and velt_rt's own tests (tests/abi_symbols.rs)
/// check that. The crates compiling the runtime's sources run their tests too.
#[test]
fn a_comment_only_runtime_change_runs_the_runtime_crates_tests() {
    let paths = vec!["crates/velt_rt/src/http/server.rs".to_string()];
    let p = Plan::for_changes(&graph(), &paths, &paths.iter().cloned().collect());
    assert_eq!(
        p.packages,
        set(&["velt_rt", "velt_rt_host", "velt_rt_shared", "velt_rt_wasm"])
    );
    assert_eq!(p.goldens, Goldens::None);
    assert_eq!(p.other_os, None);
}

#[test]
fn crates_reading_other_crates_sources_run_with_them() {
    let p = plan(&["crates/velt_rt_wasm/src/lib.rs"]);
    assert!(p.packages.contains("velt_rt"), "{:?}", p.packages);
    let p = plan(&["crates/velt_rt/src/str/mod.rs"]);
    assert!(p.packages.contains("velt_rt_wasm") && p.packages.contains("velt_rt_host"));
}
