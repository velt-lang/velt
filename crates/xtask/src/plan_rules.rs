//! What each kind of file selects: the lists behind [`super::Plan`].

/// Crates whose change can change what any compiled Velt program does: every end-to-end test
/// runs. (A crate not listed here or in [`TOOLING`] is treated like these.)
pub(crate) const PIPELINE: &[&str] = &[
    "velt_common",
    "velt_syntax",
    "velt_sema",
    // Which modules a program is made of (std, packages, the globals it loads).
    "velt_loader",
    "velt_vir",
    "velt_codegen_cl",
    "velt_codegen_llvm",
    "velt_opt",
    "velt_link",
    "velt_rt",
    "velt_rt_host",
    "velt_rt_shared",
    "velt_native",
    "velt_native_macros",
    "veltc",
];

/// The code that describes variables for debuggers: a change checks every golden with it.
pub(crate) const DEBUG_VARS_CODE: &[&str] = &[
    "crates/velt_vir/src/lower/debug_vars.rs",
    "crates/velt_vir/src/verify/debug.rs",
    "crates/velt_vir/src/vir/debug.rs",
    "crates/veltc/tests/debug_vars.rs",
];

/// The code that loads std on demand: a change compares every golden's VIR in the merge queue too,
/// where any other change that can reach the comparison checks a sample
/// ([`Plan::vir_identity_queue`]). Directories end in `/`.
pub(crate) const VIR_IDENTITY_CODE: &[&str] = &[
    "crates/velt_loader/",
    "crates/velt_sema/src/body/lazy_std/",
    "crates/velt_common/src/std_hooks.rs",
    "crates/veltc/src/driver.rs",
    "crates/veltc/src/driver/recover.rs",
    "crates/veltc/src/driver/std_load.rs",
    "crates/veltc/tests/eager_std_identity.rs",
    "std/lang/hooks/",
];

/// Crates whose change can make a program's VIR depend on when std was loaded: they read the
/// sources, number what sema makes, or lower it. `vpm` does the loader's file reads and reads
/// std's import map, and `velt_build_cache` holds std's parsed files and index.
pub(crate) const VIR_IDENTITY_CRATES: &[&str] = &[
    "velt_build_cache",
    "velt_common",
    "velt_syntax",
    "velt_sema",
    "velt_loader",
    "velt_vir",
    "vpm",
];

/// Crates that don't change compiled programs, with the `veltc` integration test binaries
/// (crates/veltc/tests/<name>.rs) that exercise them. `veltc`'s unit tests always run with them.
pub(crate) const TOOLING: &[(&str, &[&str])] = &[
    ("velt_lsp", &[]),
    ("velt_doc", &[]),
    ("velt_tscompat", &["ts_compat", "lint"]),
    ("velt_lint", &["lint", "ts_compat"]),
    ("velt_fmt", &["templates", "cli_package"]),
    (
        "vpm",
        &[
            "cli_package",
            "registry_cli",
            "native_packages",
            "templates",
            "install_layout",
            // `velt check --ts-compat` in a package reads `tsCompat` from the manifest.
            "ts_compat",
            // `velt check --perf` reads `lint` and `tsCompat` from the manifest.
            "lint",
            // `velt test --locked` resolves each example app as a package.
            "example_apps",
            // `velt test` in each of the repository's packages (path dependencies, native code).
            "packages",
        ],
    ),
    ("velt_registry", &["registry_cli", "cli_package"]),
    ("velt_http", &["registry_cli", "cli_package"]),
    // `velt target add`, and the `velt` field `velt new` writes and every command checks.
    (
        "velt_toolchain",
        &["cli_package", "templates", "install_layout"],
    ),
    // A separate binary; its tests run the launcher against fake releases.
    ("velt_launcher", &[]),
    (
        "velt_rt_wasm",
        &[
            "wasm_goldens",
            "playground",
            "workers",
            "cloudflare",
            "workerd",
        ],
    ),
];

/// Packages that compile or read a crate's sources by path, which the dependency graph doesn't
/// show (`[lib] path`, `build = …`, `#[path]`, tests reading the sources as text): a change to
/// the crate, comments included, runs their tests too.
///
/// velt_common's `hook_replaces` test reads the sources of the crates that emit code (a use of a
/// runtime extern a std hook replaces) and velt_rt's (the externs' definitions), #2197.
pub(crate) const SHARED_SOURCES: &[(&str, &[&str])] = &[
    (
        "velt_rt",
        &[
            "velt_rt_host",
            "velt_rt_shared",
            "velt_rt_wasm",
            "velt_common",
        ],
    ),
    // velt_rt's std_externs test reads velt_rt_wasm's `extern "C"` definitions.
    ("velt_rt_wasm", &["velt_rt"]),
    // velt_codegen_llvm's tests build their programs with velt_opt's test builders.
    ("velt_opt", &["velt_codegen_llvm", "velt_common"]),
    ("velt_sema", &["velt_common"]),
    ("velt_vir", &["velt_common"]),
    ("velt_codegen_llvm", &["velt_common"]),
    ("velt_codegen_cl", &["velt_common"]),
];

/// `vpm` resolves imports and packages for every program, so it runs the goldens too.
pub(crate) const GOLDENS_TOO: &[&str] = &["vpm"];

/// Files that build WebAssembly besides those with `wasm` in their path (crates/velt_rt_wasm,
/// crates/velt_link/src/wasm.rs, the `wasm_goldens` test): a change to them runs the
/// WebAssembly goldens ([`Plan::wasm`]).
pub(crate) const WASM_TOO: &[&str] = &[
    "crates/velt_codegen_llvm/src/target.rs",
    "crates/velt_codegen_llvm/src/llc.rs",
];

/// Changes to these select everything.
pub(crate) const EVERYTHING: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    ".config/",
    ".github/workflows/",
    ".github/actions/",
    "scripts/",
    "crates/xtask/",
];

/// The differential tester (`tests/difftest`): a crate of its own outside the workspace, which
/// drives the `velt` binary as a black box and depends on no workspace crate. Its changes need
/// only its own build, lints and unit tests.
pub(crate) const DIFFTEST: &str = "tests/difftest/";

/// The VS Code extension, a Node package outside the Rust workspace.
pub(crate) const VSCODE: &str = "editors/vscode/";

/// Changes to these need no check.
pub(crate) const NOTHING: &[&str] = &[
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "NOTICE",
    "CODE_OF_CONDUCT.md",
    "CONTRIBUTING.md",
    "CLAUDE.md",
    "ROADMAP.md",
    "SECURITY.md",
    ".gitignore",
    ".gitattributes",
    ".github/CODEOWNERS",
    ".github/pull_request_template.md",
    ".github/ISSUE_TEMPLATE/",
    ".claude/",
];
