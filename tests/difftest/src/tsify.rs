//! The normalizer: turns a `.vlt` program of the shared subset into the TypeScript file Node runs.
//!
//! Node's `--experimental-transform-types` already erases every annotation (`i64`, `usize`,
//! `x as f64` are just types to it) and compiles `enum`s, so the transform is deliberately tiny —
//! anything bigger would be a second compiler to debug. The documented rewrites are:
//! 1. Velt runs `main()` implicitly; JS needs the call. A numeric return becomes the exit code.
//! 2. `panic(msg)` (a Velt builtin) gets a JS shim with the same observable behavior: `panic: msg`
//!    on stderr and exit code 101.
//! 3. `import … from "velt:<module>"` loads the module's Node twin, `shims/std/<module>.ts`: the
//!    same API on Node's own implementation (`URL`, `RegExp`, `Buffer`, `node:crypto`, `Date`)
//!    where one exists, so the Velt standard library is checked against it.
//!
//! `console.log` keeps Node's default layout: Velt breaks long values across lines and groups
//! arrays into columns as `util.inspect` does (`docs/reference/builtins.md`).
//!
//! Everything else must already mean the same thing in both languages; `README.md` lists the
//! semantic differences programs have to avoid.

/// Appended to every program: runs `main` and turns an `i32` result into the exit code. Stdout is
/// flushed by Node before exit because `process.exitCode` (not `process.exit`) is used.
const ENTRY: &str = "\n\
const __veltExit: unknown = main();\n\
if (typeof __veltExit === \"number\") process.exitCode = __veltExit;\n";

const PANIC_SHIM: &str = "\nfunction panic(msg: string): never {\n  \
console.error(`panic: ${msg}`);\n  process.exit(101);\n}\n";

/// Returns the TypeScript twin of `src`.
pub fn to_typescript(src: &str) -> String {
    let mut ts = String::with_capacity(src.len() + ENTRY.len() + PANIC_SHIM.len());
    ts.push_str(&with_std_shims(src));
    if uses_builtin_panic(src) {
        ts.push_str(PANIC_SHIM);
    }
    ts.push_str(ENTRY);
    ts
}

/// Where the Node twins of std modules live (a `file:` URL prefix, `/` separated).
fn shim_prefix() -> String {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/shims/").replace('\\', "/");
    match dir.starts_with('/') {
        true => format!("file://{dir}"),
        false => format!("file:///{dir}"),
    }
}

/// `src` with every `from "velt:<module>"` pointing at the module's shim.
fn with_std_shims(src: &str) -> String {
    const FROM: &str = "from \"velt:";
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find(FROM) {
        let module_start = at + FROM.len();
        let Some(len) = rest[module_start..].find('"') else {
            break;
        };
        out.push_str(&rest[..at]);
        let module = &rest[module_start..module_start + len];
        out.push_str(&format!("from \"{}std/{module}.ts\"", shim_prefix()));
        rest = &rest[module_start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// True when the program calls `panic(...)` without defining its own `panic`.
fn uses_builtin_panic(src: &str) -> bool {
    src.contains("panic(") && !src.contains("function panic(")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_entry_call() {
        let ts = to_typescript("function main() {}\n");
        assert!(ts.starts_with("function main() {}\n"));
        assert!(ts.contains("main();"));
        assert!(!ts.contains("function panic"));
    }

    /// Velt lays out `console.log` like Node's default `util.inspect` (#496): the twin must
    /// not change Node's inspect options.
    #[test]
    fn keeps_nodes_console_layout() {
        let ts = to_typescript("function main() {}\n");
        assert!(!ts.contains("inspect"), "{ts}");
    }

    #[test]
    fn std_imports_load_their_shims() {
        let ts = to_typescript("import { URL } from \"velt:url\";\nconst a = \"from x\";\n");
        let line = ts.lines().next().unwrap_or_default();
        assert!(line.starts_with("import { URL } from \"file://"), "{line}");
        assert!(line.ends_with("/shims/std/url.ts\";"), "{line}");
        assert!(ts.contains("const a = \"from x\";"));
    }

    #[test]
    fn shims_panic_only_when_used_and_undefined() {
        assert!(to_typescript("function main() { panic(\"x\"); }").contains("function panic"));
        let own = "function panic(m: string) {}\nfunction main() { panic(\"x\"); }";
        assert_eq!(to_typescript(own).matches("function panic").count(), 1);
    }
}
