//! The options several commands share, as `velt <command> --help` lists them.

pub(super) const RELEASE: (&str, &str) = (
    "--release",
    "optimize (LLVM when clang is found) and link without debug info",
);
pub(super) const DEBUG_INFO: (&str, &str) = ("-g", "keep debug info in a --release build");
pub(super) const TARGET: (&str, &str) = (
    "--target <triple>",
    "target triple (default: the host); wasm32-wasip1 and wasm32-unknown-unknown build WebAssembly",
);
pub(super) const CPU: (&str, &str) = (
    "--cpu <name>",
    "baseline | x86-64-v2 (the x86_64 default) | x86-64-v3 | x86-64-v4 | native | a CPU name such as znver4 or neoverse-n1 (else $VELT_CPU, then build.cpu in package.vlt)",
);
pub(super) const BACKEND: (&str, &str) = (
    "--backend <name>",
    "cranelift | llvm (default: llvm for --release when clang is found)",
);
pub(super) const LOCKED: (&str, &str) = ("--locked", "fail if velt.lock.json would change");
pub(super) const VERBOSE: (&str, &str) = ("-v, --verbose", "print per-stage timings to stderr");
pub(super) const TIMINGS: (&str, &str) = (
    "--timings",
    "like -v, plus the time of each optimizer pass and codegen step",
);
pub(super) const REPORT: (&str, &str) = (
    "--report numbers",
    "list the `number` variables in loops that stay doubles (not integers), and why",
);
pub(super) const WORKSPACE: (&str, &str) = (
    "--workspace",
    "every member of the workspace (the default at its root, outside its packages)",
);
pub(super) const PACKAGE: (&str, &str) = (
    "-p, --package <glob>",
    "the workspace's members whose name matches; repeatable",
);
pub(super) const AFFECTED: (&str, &str) = (
    "--affected [<base>]",
    "the members a change since <base> touched, and those that use them (default base: the \
     merge base with the upstream branch; name it in CI)",
);
pub(super) const FAIL_FAST: (&str, &str) = ("--fail-fast", "start no more members once one fails");
pub(super) const MEMBER: (&str, &str) =
    ("-p, --package <member>", "run that member of the workspace");
pub(super) const TEMPLATE: (&str, &str) = (
    "--template <name>",
    "app | cli | api | websocket | lib (default: app)",
);
