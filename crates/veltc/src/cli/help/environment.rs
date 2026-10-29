//! The environment variables `velt --help` lists.

/// Each variable and what it does.
pub(super) const ENVIRONMENT: &[(&str, &str)] = &[
    ("VELT_STD", "standard library directory"),
    ("VELT_HOME", "vpm home (default ~/.velt: cache/, registry/)"),
    (
        "VELT_REGISTRY",
        "package registry: a directory (default $VELT_HOME/registry) or an http(s):// URL",
    ),
    (
        "VELT_REGISTRY_TOKEN",
        "a registry token for every registry server, overriding `velt login` (for CI)",
    ),
    (
        "VELT_CA_FILE",
        "PEM file of extra CA certificates to trust for https:// registries",
    ),
    (
        "VELT_CLANG",
        "clang used by the LLVM backend (default: PATH, then the standard LLVM install)",
    ),
    (
        "VELT_RT_LIB",
        "runtime library (default: next to velt, or <prefix>/lib when installed)",
    ),
    (
        "VELT_RT_LINK",
        "`static`: debug builds link the static runtime instead of the shared one",
    ),
    ("VELT_LINKER", "`bundled`, `system`, or a linker program"),
    (
        "VELT_BUILD_CACHE",
        "compiled code shared by every build on this machine (default $VELT_HOME/build-cache); \
         `off` turns it off",
    ),
    (
        "VELT_BUILD_CACHE_SIZE",
        "how large the build cache may grow (default 2G)",
    ),
    (
        "VELT_BUILD_STAMP",
        "`off`: compile even when nothing changed since the last build of the same output",
    ),
    (
        "VELT_LLVM_BIN",
        "LLVM bin directory with opt/llc for WebAssembly (default: rustup's llvm-tools)",
    ),
    (
        "VELT_WASI_SYSROOT",
        "directory with wasi-libc's crt1-command.o and libc.a (default: rustup's)",
    ),
    (
        "VELT_WASM_RUNNER",
        "program running wasm32-wasip1 modules for `velt run` (default: wasmtime)",
    ),
    (
        "VELT_TOOLCHAIN",
        "the toolchain the launcher runs for this command: a version or a linked name",
    ),
    (
        "VELT_TOOLCHAIN_AUTO_INSTALL",
        "`0`: the launcher does not install a missing pinned version",
    ),
    (
        "VELT_INSTALL_BASE_URL",
        "where toolchains and target packs are downloaded from (a mirror)",
    ),
    (
        "VELT_INSTALL_PUBLIC_KEY",
        "keys release signatures are checked with instead of velt's (another build's releases)",
    ),
    ("NO_COLOR", "set to disable colored output"),
];
