use super::*;

#[path = "plan_identity_tests.rs"]
mod identity_rules;
#[path = "plan_sweep_tests.rs"]
mod sweep_rules;

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
        krate("velt_loader", &[]),
        krate("velt_vir", &[]),
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
fn the_header_check_runs_only_when_a_change_touches_what_it_checks() {
    for path in [
        "crates/veltc/src/native/headers/ctype.rs",
        "crates/veltc/src/native/c_decls.rs",
        "crates/veltc/src/native/mod.rs",
        "crates/veltc/tests/c_headers.rs",
        "crates/veltc/tests/c_constants.rs",
        "crates/veltc/tests/c_sources.rs",
        "crates/veltc/tests/zig_env/mod.rs",
        "crates/velt_toolchain/src/zig.rs",
        "crates/vpm/src/native/c_build.rs",
        "crates/vpm/src/install/c_sources.rs",
        "crates/vpm/src/manifest/read/native.rs",
        "crates/vpm/src/install/allow_native.rs",
        "crates/vpm/src/manifest/allow_native.rs",
        "std/ffi/out.velt",
        ".github/actions/zig/action.yml",
    ] {
        assert!(plan(&[path]).c_headers, "{path}");
    }
    // Unrelated changes never download zig, even those that select everything else.
    for path in [
        "crates/velt_sema/src/check.rs",
        "crates/veltc/src/main.rs",
        "crates/veltc/src/commands/mod.rs",
        "crates/veltc/tests/c_functions.rs",
        "crates/veltc/tests/c_headers.rs.orig",
        "crates/veltc/tests/c_sources_more.rs",
        "crates/vpm/src/native/build.rs",
        "crates/velt_toolchain/src/lib.rs",
        "crates/vpm/src/manifest/read.rs",
        "std/fs/index.velt",
        "docs/internals/design/c-ffi.md",
        "Cargo.lock",
        "scripts/check.sh",
        ".github/workflows/ci.yml",
        ".github/actions/setup/action.yml",
        "something/new.txt",
    ] {
        assert!(!plan(&[path]).c_headers, "{path}");
    }
    assert!(!Plan::everything("--full").c_headers);
    // A change that selects everything still runs it when it touches the check too, whichever
    // file comes first.
    assert!(plan(&["Cargo.lock", "std/ffi/out.velt"]).c_headers);
    assert!(plan(&["std/ffi/out.velt", "Cargo.lock"]).c_headers);
    // Comments change nothing it checks.
    let paths = vec!["crates/veltc/src/native/headers/ctype.rs".to_string()];
    let comments = Plan::for_changes(&graph(), &paths, &paths.iter().cloned().collect());
    assert!(!comments.c_headers);
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
    assert_eq!(p.packages, set(&["velt_common", "velt_lsp", "velt_sema"]));
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
    assert_eq!(
        p.filterset().unwrap(),
        "(package(velt_common) | package(velt_lsp) | package(velt_sema) | package(veltc)) - \
         binary_id(veltc::golden) - binary_id(veltc::wasm_goldens)"
    );
}

#[test]
fn the_runtime_has_no_dependents_but_runs_every_end_to_end_test() {
    let p = plan(&["crates/velt_rt/src/http.rs"]);
    // The crates compiling the runtime's sources run their tests too.
    assert_eq!(
        p.packages,
        set(&[
            "velt_common",
            "velt_rt",
            "velt_rt_host",
            "velt_rt_shared",
            "velt_rt_wasm"
        ])
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
    assert!(!p.velt_fmt);
    assert_eq!(
        p.filterset().unwrap(),
        "(package(velt_lsp) | (package(veltc) & (kind(lib) | kind(bin) | binary(standards)))) \
         - binary_id(veltc::golden)"
    );
    let wasm = plan(&["crates/velt_rt_wasm/src/lib.rs"]);
    assert_eq!(
        wasm.veltc,
        Veltc::Some(set(&[
            "cloudflare",
            "playground",
            "wasm_goldens",
            "workerd",
            "workers"
        ]))
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
fn the_formatter_reaches_its_dependents_and_the_velt_format_check() {
    let p = plan(&["crates/velt_fmt/src/jsx.rs"]);
    assert_eq!(p.packages, set(&["velt_fmt", "velt_lsp", "vpm"]));
    assert!(p.velt_fmt);
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
    let p = plan(&["packages/redis/src/client.velt"]);
    assert_eq!(
        p.veltc,
        Veltc::Some(set(&[
            "packages",
            "native_packages",
            "example_apps",
            "bench_programs",
            "docs"
        ]))
    );
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.filterset().unwrap().contains("binary(packages)"));
    // Its Velt files are formatted like std's; its native crate is linted only when it changed.
    assert!(p.velt_fmt && !p.native_crates);
    let p = plan(&["packages/postgres/native/src/lib.rs"]);
    assert!(p.native_crates);
    assert!(!p.rust, "no workspace crate changed");
}

/// packages/http-server is used by goldens, the standard library's tests, benchmarks and the
/// documentation: a change to it runs all of them.
#[test]
fn a_changed_http_server_runs_what_depends_on_it() {
    let p = plan(&["packages/http-server/src/h2/request.velt"]);
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
        "tests/golden/lang/foo.velt",
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
        filter("tests/golden/lang/_helper.velt"),
        "tests/golden/lang/"
    );
    assert_eq!(
        filter("tests/golden/lang/_mod/index.velt"),
        "tests/golden/lang/"
    );
    assert_eq!(
        filter("tests/golden/lang/errors/_p/x.velt"),
        "tests/golden/lang/"
    );
    assert_eq!(filter("tests/golden/bugs/.pending"), "tests/golden/bugs/");
    assert_eq!(
        filter("tests/golden/lang/jsx_package/pages/a.velt"),
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
    assert_eq!(filter("examples/apps/todo/main.velt"), "examples/apps/");
}

#[test]
fn the_standard_library_runs_every_test_but_no_clippy() {
    let p = plan(&["std/encoding/json/index.velt"]);
    assert!(!p.full && !p.rust);
    assert!(p.packages.contains("velt_rt") && p.packages.contains("velt_fmt"));
    assert!(!p.packages.contains("veltc"));
    assert_eq!(
        p.filterset().unwrap(),
        "all() - binary_id(veltc::golden) - binary_id(veltc::wasm_goldens)"
    );
    assert_eq!(p.veltc, Veltc::All);
    assert_eq!(p.goldens, Goldens::All);
    assert!(p.velt_fmt);
}

#[test]
fn the_readme_runs_the_documentation_tests() {
    let p = plan(&["README.md"]);
    assert!(p.packages.is_empty());
    assert_eq!(p.veltc, Veltc::Some(set(&["docs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.needs_build());
}

/// The user docs live on the website; of docs/, only the runtime ABI contracts are read by a test.
#[test]
fn internals_docs_run_only_the_tests_that_read_them() {
    let p = plan(&["docs/internals/contracts/rt_abi.md"]);
    assert_eq!(p.packages, set(&["velt_rt_host"]));
    for path in ["docs/internals/design/semantics.md", "docs/README.md"] {
        let p = plan(&[path]);
        assert!(p.packages.is_empty() && !p.needs_build(), "{path}");
    }
}

/// The benchmarks compile whenever they, the compiler or the standard library change (#429).
#[test]
fn benchmarks_are_compiled_when_they_the_compiler_or_std_change() {
    let p = plan(&["bench/db/velt/postgres.velt"]);
    assert_eq!(p.packages, set(&["velt_common", "velt_sema"]));
    assert_eq!(p.veltc, Veltc::Some(set(&["bench_programs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(p.filterset().unwrap().contains("binary(bench_programs)"));
    // Every veltc test binary, `bench_programs` among them, runs for these.
    assert_eq!(plan(&["std/db/index.velt"]).veltc, Veltc::All);
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
        "README.md",
        "crates/velt_lsp/src/a.rs",
        "tests/golden/std/fs.velt",
    ]);
    assert!(p.packages.contains("velt_lsp"));
    let Veltc::Some(binaries) = &p.veltc else {
        panic!("{:?}", p.veltc)
    };
    assert!(binaries.contains("docs") && binaries.contains("wasm_goldens"));
    assert_eq!(p.golden_env().unwrap(), "tests/golden/std/fs.");
    // A pipeline crate on top makes the end-to-end tests run in full.
    let more = plan(&["README.md", "crates/velt_syntax/src/lexer.rs"]);
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
        "tests/difftest/corpus/knapsack.velt",
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
        "README.md".to_string(),
    ];
    let p = Plan::for_changes(&graph(), &paths, &paths[..2].iter().cloned().collect());
    assert!(!p.full && p.rust);
    assert_eq!(
        p.packages,
        set(&[
            "velt_common",
            "velt_rt",
            "velt_rt_host",
            "velt_rt_shared",
            "velt_rt_wasm",
            "velt_sema"
        ])
    );
    assert_eq!(p.veltc, Veltc::Some(set(&["docs"])));
    assert_eq!(p.goldens, Goldens::None);
    assert!(!p.velt_fmt);
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
        set(&[
            "velt_common",
            "velt_rt",
            "velt_rt_host",
            "velt_rt_shared",
            "velt_rt_wasm"
        ])
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
    // An app's tests or sources: its tests (`velt test`) and its goldens (`demo.velt`).
    let p = plan(&["examples/apps/chat/tests/room.test.velt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["templates", "example_apps"])));
    assert_eq!(p.goldens, Goldens::Matching(set(&["examples/apps/"])));
    assert!(p.filterset().unwrap().contains("binary(example_apps)"));
    // The compiler, the runtime and the standard library run every `veltc` test binary,
    // `example_apps` with them.
    for path in [
        "crates/velt_sema/src/lib.rs",
        "crates/velt_rt/src/lib.rs",
        "std/fs/index.velt",
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
    let p = plan(&["tests/std/fetch/client.test.velt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["std_tests"])));
    assert!(p.filterset().unwrap().contains("binary(std_tests)"));
    // A change to std or the runner runs every `veltc` test, `std_tests` with them.
    assert_eq!(plan(&["std/test/index.velt"]).veltc, Veltc::All);
    // std's white-box tests run only with `std_tests` (`velt test std/`), no goldens.
    let p = plan(&["std/encoding/json/scan.test.velt"]);
    assert_eq!(p.veltc, Veltc::Some(set(&["std_tests"])));
    assert_eq!(p.goldens, Goldens::None);
    // They are formatted and linted with std.
    assert!(p.velt_fmt);
    assert!(!plan(&["tests/std/fetch/client.test.velt"]).velt_fmt);
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
fn the_webassembly_goldens_run_only_for_webassembly_code() {
    let skipped = " - binary_id(veltc::wasm_goldens)";
    for paths in [
        &["crates/velt_sema/src/check.rs"][..],
        &["crates/velt_rt/src/http.rs"],
        &["std/encoding/json/index.velt"],
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
        &["tests/golden/lang/foo.velt"][..],
        &["crates/velt_rt_wasm/src/lib.rs"],
    ] {
        let filterset = plan(paths).filterset().unwrap();
        assert!(filterset.contains("binary(wasm_goldens)"), "{filterset}");
        assert!(!filterset.contains(skipped), "{filterset}");
    }
    assert!(Plan::everything("--full").wasm);
}

#[path = "plan_os_tests.rs"]
mod os;

#[test]
fn zig_is_selected_for_the_tests_that_build_c_sources() {
    for path in [
        "packages/sqlite/src/connection.velt",
        "packages/sqlite/native/sqlite3.c",
        "packages/sqlite/tests/driver.test.velt",
        "examples/apps/todo-api/src/sqlite_store.velt",
        "bench/sqlite/engine.velt",
        "bench/chat/velt/server.velt",
        // Every package's tests run, sqlite's among them.
        "packages/redis/src/client.velt",
        // Everything.
        "std/fs/index.velt",
        "Cargo.lock",
    ] {
        assert!(crate::zig::tests(&plan(&[path])), "{path}");
    }
    assert!(crate::zig::tests(&Plan::everything("--full")));
    for path in [
        "examples/apps/todo-api/demo.velt",
        "examples/apps/todo-api/demo_sqlite.out",
        "examples/apps/todo-api/src/server.velt",
    ] {
        assert!(crate::zig::goldens(&plan(&[path])), "{path}");
    }
    // A change to any example app runs every example app's goldens.
    assert!(crate::zig::goldens(&plan(&[
        "examples/apps/chat/demo.velt"
    ])));
    assert!(crate::zig::goldens(&Plan::everything("--full")));
}

#[test]
fn zig_is_not_selected_for_unrelated_changes() {
    for path in [
        "crates/velt_lsp/src/lib.rs",
        "tests/golden/lang/errors/std_moved_modules.velt",
        "docs/internals/design/c-ffi.md",
        "editors/vscode/package.json",
        "tests/difftest/src/main.rs",
    ] {
        assert!(!crate::zig::tests(&plan(&[path])), "{path}");
    }
    for path in [
        "packages/sqlite/src/connection.velt",
        "bench/sqlite/engine.velt",
        "examples/wordfreq.velt",
        "examples/wordfreq.out",
        "tests/golden/lang/errors/std_moved_modules.velt",
    ] {
        assert!(!crate::zig::goldens(&plan(&[path])), "{path}");
    }
}

/// The goldens that build C `sources` find each bundle built by a step before them, in the
/// package's directory, where their builds look; other goldens build none.
#[test]
fn the_c_bundles_are_built_before_the_goldens_that_link_them() {
    let root = std::path::Path::new("/repo");
    let velt = crate::check::target_dir(root)
        .join("debug")
        .join(crate::check::exe("velt"));
    for plan in [
        Plan::everything("--full"),
        plan(&["examples/apps/todo-api/demo_sqlite.velt"]),
    ] {
        let builds = crate::zig::bundle_builds(root, &plan);
        assert_eq!(builds.len(), crate::zig::PACKAGES.len());
        for ((_, build), p) in builds.iter().zip(crate::zig::PACKAGES) {
            assert_eq!(build.get_program(), velt.as_os_str());
            let args: Vec<_> = build.get_args().collect();
            assert_eq!(args, ["native", "build"]);
            let dir = root.join("packages").join(p);
            assert_eq!(build.get_current_dir(), Some(dir.as_path()));
        }
    }
    for path in [
        "tests/golden/lang/errors/std_moved_modules.velt",
        "bench/sqlite/engine.velt",
    ] {
        assert!(
            crate::zig::bundle_builds(root, &plan(&[path])).is_empty(),
            "{path}"
        );
    }
}
