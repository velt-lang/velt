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
        krate("velt_toolchain", &["velt_syntax"]),
        krate("velt_launcher", &["velt_toolchain"]),
        krate("vpm", &["velt_fmt", "velt_toolchain"]),
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
        "(package(velt_lsp) | package(velt_sema) | package(veltc)) - binary_id(veltc::golden) \
         - binary_id(veltc::wasm_goldens)"
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
fn the_toolchain_crate_runs_with_vpm_on_every_os() {
    let p = plan(&["crates/velt_toolchain/src/release.rs"]);
    assert_eq!(p.packages, set(&["velt_launcher", "velt_toolchain", "vpm"]));
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    assert!(binaries.contains("cli_package") && binaries.contains("install_layout"));
    // It unpacks files and renames directories, which differ by OS.
    assert!(p.other_os.is_some());
    // The launcher depends on it but has no veltc tests of its own.
    let launcher = plan(&["crates/velt_launcher/src/select.rs"]);
    assert_eq!(launcher.veltc, Veltc::Some(set(&[])));
    assert_eq!(launcher.goldens, Goldens::None);
    assert!(launcher.other_os.is_some(), "exec and Ctrl-C differ by OS");
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

/// A change in packages/ runs `velt test` in the packages, the registry flow of packages/sqlite,
/// the example apps (which depend on packages by path) and the benchmarks using the package; the
/// goldens only when some depend on the package.
#[test]
fn a_changed_package_runs_the_package_tests() {
    let p = plan(&["packages/redis/src/client.vlt"]);
    assert_eq!(
        p.veltc,
        Veltc::Some(set(&[
            "packages",
            "native_packages",
            "example_apps",
            "bench_programs"
        ]))
    );
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.filterset().unwrap().contains("binary(packages)"));
    // Its Velt files are formatted like std's; its native crate is linted only when it changed.
    assert!(p.vlt_fmt && !p.native_crates);
    let p = plan(&["packages/redis/native/src/resp.rs"]);
    assert!(p.native_crates);
    assert!(!p.rust, "no workspace crate changed");
}

/// packages/http-server is used by goldens, the standard library's tests, benchmarks and the
/// documentation: a change to it runs all of them.
#[test]
fn a_changed_http_server_runs_what_depends_on_it() {
    let p = plan(&["packages/http-server/src/h2/request.vlt"]);
    assert_eq!(
        p.golden_env().unwrap(),
        "tests/golden/lang/errors/http_server/,tests/golden/std/http_server/"
    );
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    for b in [
        "packages",
        "example_apps",
        "std_tests",
        "bench_programs",
        "docs",
        "reload",
        "templates",
    ] {
        assert!(binaries.contains(b), "{b}: {binaries:?}");
    }
    let p = plan(&["packages/http-server/README.md"]);
    assert!(matches!(&p.veltc, Veltc::Some(b) if b.contains("docs")));
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
    let p = plan(&["std/encoding/json/index.vlt"]);
    assert!(!p.full && !p.rust);
    assert!(p.packages.contains("velt_rt") && p.packages.contains("velt_fmt"));
    assert!(!p.packages.contains("veltc"));
    assert_eq!(
        p.filterset().unwrap(),
        "all() - binary_id(veltc::golden) - binary_id(veltc::wasm_goldens)"
    );
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

/// The benchmarks compile whenever they, the compiler or the standard library change (#429).
#[test]
fn benchmarks_are_compiled_when_they_the_compiler_or_std_change() {
    let p = plan(&["bench/db/velt/postgres.vlt"]);
    assert_eq!(p.packages, set(&["velt_sema"]));
    assert_eq!(p.veltc, Veltc::Some(set(&["bench_programs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.filterset().unwrap().contains("binary(bench_programs)"));
    // Every veltc test binary, `bench_programs` among them, runs for these.
    assert_eq!(plan(&["std/db/index.vlt"]).veltc, Veltc::All);
    assert_eq!(plan(&["crates/velt_sema/src/check.rs"]).veltc, Veltc::All);
}

#[test]
fn the_tsc_oracle_runs_the_lint_tests() {
    let p = plan(&["tests/tscompat-oracle/rejected/struct.ts"]);
    assert_eq!(p.packages, set(&["velt_tscompat"]));
    assert_eq!(p.veltc, Veltc::Some(set(&["ts_compat"])));
    assert_eq!(p.goldens, Goldens::None);
}

#[test]
fn the_lldb_script_runs_the_install_layout_and_debugger_tests() {
    let p = plan(&["editors/lldb/velt_lldb.py"]);
    assert!(p.packages.is_empty());
    assert_eq!(p.veltc, Veltc::Some(set(&["install_layout", "debugger"])));
    assert_eq!(p.goldens, Goldens::None);
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
        "std/fs/index.vlt",
        "tests/golden/lang/foo.out",
        // The standard library's `velt test` suites run servers, sockets and files (#1892).
        "tests/std/http/responses.test.vlt",
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

#[test]
fn example_apps_run_their_velt_tests() {
    // An app's tests or sources: its tests (`velt test`) and its goldens (`demo.vlt`).
    let p = plan(&["examples/apps/chat/tests/room.test.vlt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["templates", "example_apps"])));
    assert_eq!(p.goldens, Goldens::Matching(set(&["examples/apps/"])));
    assert!(p.filterset().unwrap().contains("binary(example_apps)"));
    // The compiler, the runtime and the standard library run every `veltc` test binary,
    // `example_apps` with them.
    for path in [
        "crates/velt_sema/src/lib.rs",
        "crates/velt_rt/src/lib.rs",
        "std/fs/index.vlt",
    ] {
        assert_eq!(plan(&[path]).veltc, Veltc::All, "{path}");
    }
    // Package resolution: `velt test --locked` resolves each app as a package.
    match plan(&["crates/vpm/src/lib.rs"]).veltc {
        Veltc::Some(binaries) => assert!(binaries.contains("example_apps")),
        other => panic!("vpm: {other:?}"),
    }
}

#[test]
fn std_suites_run_with_std_tests() {
    let p = plan(&["tests/std/fetch/client.test.vlt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["std_tests"])));
    assert!(p.filterset().unwrap().contains("binary(std_tests)"));
    // A change to std or the runner runs every `veltc` test, `std_tests` with them.
    assert_eq!(plan(&["std/test/index.vlt"]).veltc, Veltc::All);
    // std's white-box tests run only with `std_tests` (`velt test std/`), no goldens.
    let p = plan(&["std/encoding/json/scan.test.vlt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["std_tests"])));
    assert_eq!(p.goldens, Goldens::None);
    // They are formatted and linted with std.
    assert!(p.vlt_fmt);
    assert!(!plan(&["tests/std/fetch/client.test.vlt"]).vlt_fmt);
}

#[test]
fn the_vscode_extension_runs_its_own_tests() {
    let p = plan(&["editors/vscode/src/debug.ts", "editors/vscode/package.json"]);
    assert!(!p.full);
    assert!(p.vscode);
    assert!(!p.rust && !p.needs_build(), "no workspace build");
    assert_eq!(p.goldens, Goldens::None);
    // The templates are read by the `debugger` test, not the extension's.
    let t = plan(&["editors/vscode/templates/launch.json"]);
    assert!(!t.vscode);
    assert_eq!(t.veltc, Veltc::Some(set(&["debugger"])));
    // Rust changes leave it alone; everything includes it.
    assert!(!plan(&["crates/velt_syntax/src/lexer.rs"]).vscode);
    assert!(plan(&["Cargo.lock"]).vscode);
}

#[test]
fn the_parser_and_its_dependencies_sweep_every_golden_in_full() {
    for path in [
        "crates/velt_syntax/src/parser/expr.rs",
        "crates/velt_syntax/tests/common/mod.rs",
        "crates/velt_syntax/Cargo.toml",
        "crates/velt_common/src/span.rs",
    ] {
        let p = plan(&[path, "tests/golden/lang/foo.vlt"]);
        assert_eq!(p.sweep, Sweep::Full, "{path}");
        assert_eq!(p.sweep_env(), [("VELT_ROBUSTNESS", "full".to_string())]);
    }
}

#[test]
fn other_changes_sweep_the_changed_goldens_in_full_and_sample_the_rest() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "std/string.vlt",
        "docs/reference/README.md",
        ".github/workflows/ci.yml",
    ] {
        assert_eq!(plan(&[path]).sweep, Sweep::Sampled(set(&[])), "{path}");
        assert!(plan(&[path]).sweep_env().is_empty(), "{path}");
    }
    let p = plan(&[
        "tests/golden/lang/foo.vlt",
        "tests/golden/lang/foo.out",
        "tests/golden/m1/errors/bar.vlt",
        "examples/hello.vlt",
        "crates/velt_sema/src/check.rs",
    ]);
    assert_eq!(
        p.sweep,
        Sweep::Sampled(set(&[
            "tests/golden/lang/foo.vlt",
            "tests/golden/m1/errors/bar.vlt"
        ]))
    );
    assert_eq!(
        p.sweep_env(),
        [(
            "VELT_ROBUSTNESS_FILES",
            "tests/golden/lang/foo.vlt,tests/golden/m1/errors/bar.vlt".to_string()
        )]
    );
    // A change that selects everything still says which goldens it touched.
    let p = plan(&["Cargo.lock", "tests/golden/lang/foo.vlt"]);
    assert!(p.full);
    assert_eq!(p.sweep, Sweep::Sampled(set(&["tests/golden/lang/foo.vlt"])));
}

#[test]
fn comment_only_parser_changes_and_many_goldens() {
    let paths = vec!["crates/velt_syntax/src/lexer/mod.rs".to_string()];
    let p = Plan::for_changes(
        &graph(),
        &paths,
        &set(&["crates/velt_syntax/src/lexer/mod.rs"]),
    );
    assert_eq!(p.sweep, Sweep::Sampled(set(&[])));
    let many: Vec<String> = (0..=MAX_SWEPT_GOLDENS)
        .map(|i| format!("tests/golden/lang/g{i}.vlt"))
        .collect();
    assert_eq!(Plan::for_paths(&graph(), &many).sweep, Sweep::Full);
    assert!(matches!(
        Plan::for_paths(&graph(), &many[1..]).sweep,
        Sweep::Sampled(files) if files.len() == MAX_SWEPT_GOLDENS
    ));
}

#[test]
fn the_toolchain_and_the_root_manifest_sweep_in_full() {
    for path in ["rust-toolchain.toml", "Cargo.toml"] {
        assert_eq!(plan(&[path]).sweep, Sweep::Full, "{path}");
    }
    // A crate's manifest other than the parser's keeps the sample.
    assert_eq!(
        plan(&["crates/velt_sema/Cargo.toml"]).sweep,
        Sweep::Sampled(set(&[]))
    );
}

#[test]
fn unknown_changes_sweep_in_full_but_explicit_full_leaves_it_to_the_environment() {
    let unknown = Plan::unknown_changes("no base revision");
    assert!(unknown.full);
    assert_eq!(unknown.sweep, Sweep::Full);
    assert_eq!(
        unknown.sweep_env(),
        [("VELT_ROBUSTNESS", "full".to_string())]
    );
    let full = Plan::everything("--full");
    assert_eq!(full.sweep, Sweep::Sampled(set(&[])));
    assert!(full.sweep_env().is_empty());
}

#[test]
fn the_webassembly_goldens_run_only_for_webassembly_code() {
    let skipped = " - binary_id(veltc::wasm_goldens)";
    for paths in [
        &["crates/velt_sema/src/check.rs"][..],
        &["crates/velt_rt/src/http.rs"],
        &["std/encoding/json/index.vlt"],
        // Changes that select everything else leave them to the nightly too.
        &["Cargo.lock"],
        &[".github/workflows/ci.yml"],
        &["scripts/check.sh"],
    ] {
        let p = plan(paths);
        assert!(!p.wasm, "{paths:?}");
        assert!(p.filterset().unwrap().ends_with(skipped), "{paths:?}");
        assert!(p.describe().contains("WebAssembly goldens: no (nightly"));
    }
    for paths in [
        &[
            "crates/velt_sema/src/check.rs",
            "crates/velt_rt_wasm/src/io.rs",
        ][..],
        &["crates/velt_link/src/wasm.rs"],
        &["crates/veltc/tests/wasm_goldens.rs"],
        &["crates/velt_codegen_llvm/src/target.rs"],
        &["rust-toolchain.toml"],
        &[".cargo/config.toml"],
        &["Cargo.lock", "crates/velt_rt_wasm/Cargo.toml"],
    ] {
        let p = plan(paths);
        assert!(p.wasm, "{paths:?}");
        assert!(!p.filterset().unwrap().contains(skipped), "{paths:?}");
        assert!(p.describe().contains("WebAssembly goldens: yes"));
    }
    // A changed golden, or the WebAssembly runtime alone, names the binary.
    for paths in [
        &["tests/golden/lang/foo.vlt"][..],
        &["crates/velt_rt_wasm/src/lib.rs"],
    ] {
        let filterset = plan(paths).filterset().unwrap();
        assert!(filterset.contains("binary(wasm_goldens)"), "{filterset}");
        assert!(!filterset.contains(skipped), "{filterset}");
    }
    assert!(Plan::everything("--full").wasm);
}

#[test]
fn main_skips_the_windows_and_macos_gate_the_queue_already_ran() {
    // The queue checked every OS: the push to main doesn't check them again.
    for path in [
        "crates/velt_rt/src/http.rs",
        "crates/veltc/src/main.rs",
        "std/fs/index.vlt",
        "tests/golden/lang/foo.out",
        ".github/workflows/ci.yml",
        "scripts/check-all.sh",
        "crates/xtask/src/plan.rs",
        "Cargo.toml",
    ] {
        let p = plan(&[path]);
        assert!(p.other_os.is_some(), "{path}");
        assert_eq!(p.after_merge, None, "{path}");
    }
    let p = plan(&[
        "crates/velt_sema/src/check.rs",
        "crates/velt_rt/src/http.rs",
    ]);
    assert_eq!(p.after_merge, None);
}

#[test]
fn main_runs_the_windows_and_macos_gate_after_a_linux_only_queue_run() {
    for path in [
        "crates/velt_sema/src/check.rs",
        "crates/velt_lsp/src/hover.rs",
        "docs/reference/types.md",
        "CONTRIBUTING.md",
    ] {
        let p = plan(&[path]);
        assert_eq!(p.other_os, None, "{path}");
        assert!(p.after_merge.is_some(), "{path}");
    }
    // Comments only in OS-specific code: the queue checked Linux only.
    let paths = vec!["crates/velt_rt/src/http/server.rs".to_string()];
    let p = Plan::for_changes(&graph(), &paths, &paths.iter().cloned().collect());
    assert!(p.after_merge.is_some());
}

#[test]
fn main_runs_the_windows_and_macos_gate_when_the_build_cache_changes() {
    // Every OS in the queue, but only a push to main saves the caches the toolchain and the
    // dependencies key.
    for path in [
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        ".github/actions/setup/action.yml",
    ] {
        let p = plan(&[path]);
        assert!(p.other_os.is_some(), "{path}");
        let reason = p.after_merge.unwrap_or_default();
        assert!(reason.contains("build cache"), "{path}: {reason}");
    }
    let p = plan(&["crates/velt_rt/src/http.rs", "Cargo.lock"]);
    assert!(p.after_merge.is_some());
}

#[test]
fn main_runs_the_windows_and_macos_gate_when_the_change_is_unknown() {
    // Nobody knows which OSes the queue checked.
    assert!(Plan::unknown_changes("no base revision")
        .after_merge
        .is_some());
    assert!(Plan::everything("--full").after_merge.is_some());
}
