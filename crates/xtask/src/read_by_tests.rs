//! Paths outside `crates/` that tests read, whose change selects those tests
//! (crates/xtask/src/plan.rs).

/// Paths outside `crates/` that tests read: (prefix, packages whose tests read it, `veltc` test
/// binaries that read it).
pub(crate) const READ_BY_TESTS: &[(&str, &[&str], &[&str])] = &[
    // velt_rt_host's ABI test reads the runtime ABI contracts; no test reads the rest of docs/
    // (below).
    ("docs/internals/contracts/", &["velt_rt_host"], &[]),
    // `docs` compiles the README's examples.
    ("README.md", &[], &["docs"]),
    // velt_sema's scaling test reads bench/compile; `bench_programs` compiles every program.
    ("bench/", &["velt_sema"], &["bench_programs"]),
    ("fuzz/", &["velt_fmt"], &[]),
    // The `tsc` oracle's samples and configuration: velt_tscompat's tests read them (and skip
    // the `tsc` run without its Node packages), `veltc`'s check that the samples are valid Velt.
    ("tests/tscompat-oracle/", &["velt_tscompat"], &["ts_compat"]),
    // The pinned Miniflare and workerd that `workerd` runs its Worker under.
    ("tests/workerd/", &[], &["workerd"]),
    // The standard library's `velt test` suites (`std_tests`; std and the compiler run it too:
    // they select every `veltc` test).
    ("tests/std/", &[], &["std_tests"]),
    // `packages` runs `velt test` in every packages/<name>/ (building its native crate);
    // `native_packages` publishes packages/sqlite through a local registry; example apps depend
    // on packages by path. What else uses a package adds its tests
    // (crates/xtask/src/package_users.rs).
    (
        "packages/",
        &[],
        &["packages", "native_packages", "example_apps"],
    ),
    ("playground/", &[], &["playground"]),
    ("editors/vscode/templates/", &[], &["debugger"]),
    // The LLDB script the toolchain ships (`install_layout` installs it and asks for its path;
    // `debugger` runs its formatters).
    ("editors/lldb/", &[], &["install_layout", "debugger"]),
    // `example_apps` runs `velt test` for examples/apps/*/ (the compiler, runtime and std run
    // it too: they select every `veltc` test).
    (
        "examples/",
        &["velt_syntax", "velt_codegen_llvm"],
        &["templates", "example_apps"],
    ),
    (
        "tests/golden/",
        &["velt_fmt", "velt_sema", "velt_syntax", "velt_vir"],
        &[
            "wasm_goldens",
            "llvm_goldens",
            "debug_info",
            "panic_locations",
        ],
    ),
];
