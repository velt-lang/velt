//! The C FFI's trust boundary (docs/internals/design/c-ffi.md "The trust boundary", #2187 step
//! 1): `unsafe` code and the raw API (`Ptr`, `OutPtr`, `declare c type`s, unsafe functions) only
//! in std and in packages with native code that the program allows, and in those each piece of
//! unsafe code has a `// SAFETY:` comment.

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

/// A package `bind` (manifest fields `native` after its version, source `lib`) and a program
/// `app` depending on it (manifest fields `allow` before its dependencies, source `main`).
fn project(dir: &Path, lib: &str, native: &str, main: &str, allow: &str) {
    let pkg = dir.join("bind");
    let app = dir.join("app");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{ name: \"bind\", version: \"0.1.0\"{native} }};\n"),
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.vlt"), lib).unwrap();
    std::fs::write(
        app.join("package.vlt"),
        format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  {allow}dependencies: {{ bind: {{ path: \"../bind\" }} }},\n}};\n"),
    )
    .unwrap();
    std::fs::write(app.join("src/main.vlt"), main).unwrap();
}

const NATIVE: &str = ", native: { c: {} }";
const ALLOW: &str = "allowNative: [\"bind\"], ";

fn velt(dir: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir.join("app"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// `velt check`'s errors (it must fail).
fn errors(dir: &Path) -> String {
    let o = velt(dir, &["check"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!o.status.success(), "expected errors:\n{out}");
    out
}

fn passes(dir: &Path, args: &[&str]) -> String {
    let o = velt(dir, args);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "velt {args:?} failed:\n{stderr}");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Unsafe blocks, expressions, functions and methods (tests/golden/lang/unsafe_code.vlt before
/// they needed a package), wrapped in a safe `demo()`.
const UNSAFE_CODE: &str = r#"// SAFETY: callers pass a non-negative `n`.
unsafe function raw(n: i64): i64 {
  return n * 2;
}

class Buf {
  items: i64[] = [1, 2, 3];

  /** The item at `i`. Unsafe: `i` is below the length. */
  unsafe at(i: i64): i64 {
    return this.items[i];
  }
}

function pick(n: i64): i64 {
  if (n > 0) {
    // SAFETY: `n` is positive.
    unsafe {
      return raw(n);
    }
  }
  return 0;
}

export function demo(): void {
  const b = new Buf();
  // SAFETY: 1 is below the length.
  const x = unsafe { b.at(1) };
  // SAFETY: 3 is non-negative.
  unsafe {
    console.log(raw(3), x);
  }
  // SAFETY: the arrow is called with lengths.
  const twice = (n: i64) => unsafe { raw(n) };
  console.log(twice(b.items.length), pick(5), pick(-1));
  const unsafe = "still a name";
  console.log(unsafe);
}
"#;

const DEMO: &str = "import { demo } from \"bind\";\n\nfunction main() {\n  demo();\n}\n";

#[test]
fn unsafe_code_runs_in_an_allowed_native_package() {
    let dir = test_dir::TestDir::new();
    project(dir.path(), UNSAFE_CODE, NATIVE, DEMO, ALLOW);
    for args in [&["run"][..], &["run", "--release"][..]] {
        assert_eq!(
            passes(dir.path(), args),
            "6 2\n6 10 0\nstill a name\n",
            "{args:?}"
        );
    }
}

#[test]
fn unsafe_code_needs_an_allowed_native_package() {
    let dir = test_dir::TestDir::new();
    // A package without native code.
    project(dir.path(), UNSAFE_CODE, "", DEMO, ALLOW);
    let out = errors(dir.path());
    assert!(
        out.contains("unsafe function `raw` needs a native package"),
        "{out}"
    );
    assert!(
        out.contains("unsafe function `at` needs a native package"),
        "{out}"
    );
    assert_eq!(
        out.matches("`unsafe` code needs a native package").count(),
        4,
        "{out}"
    );
    assert!(out.contains("package `bind` has no native code"), "{out}");
    // Native code the program doesn't allow: refused when it is installed, before any build.
    project(dir.path(), UNSAFE_CODE, NATIVE, DEMO, "");
    let out = errors(dir.path());
    assert!(
        out.contains("package `bind` has native code, which runs with no safety net"),
        "{out}"
    );
    // The program's own code, also hidden in a parameter's default.
    let main = "// SAFETY: none.\nunsafe function mine(): i64 {\n  return 1;\n}\n\nfunction f(x: i64 = unsafe { mine() }): i64 {\n  return x;\n}\n\nfunction main() {\n  console.log(f());\n}\n";
    project(dir.path(), UNSAFE_CODE, NATIVE, main, ALLOW);
    let out = errors(dir.path());
    assert!(
        out.contains("main.vlt:2:17: error: unsafe function `mine` needs a native package"),
        "{out}"
    );
    assert!(
        out.contains("main.vlt:6:21: error: `unsafe` code needs a native package"),
        "{out}"
    );
    assert!(out.contains("package `app` has no native code"), "{out}");
}

/// A binding over raw API: `Ptr`, a `declare c type`, an unsafe function, re-exported.
const RAW_LIB: &str = r#"import { Ptr } from "velt/ffi";

export { Ptr, readCString } from "velt/ffi";

export declare c type FILE;

declare c("c") function getenv(name: Ptr<u8> | null): Ptr<u8> | null;

/** NULL's environment entry. Unsafe: none. */
export unsafe function nothing(): Ptr<u8> | null {
  return getenv(null);
}

export function safe(): i64 {
  return 1;
}
"#;

#[test]
fn raw_api_is_imported_only_by_allowed_native_packages() {
    let dir = test_dir::TestDir::new();
    let main = "import { CString, Ptr, readCString } from \"velt/ffi\";\nimport * as ffi from \"velt/ffi\";\nimport { FILE, nothing, safe, Ptr as P } from \"bind\";\n\nfunction f(p: ffi.Ptr<u8>, q: Ptr<u8>, r: P<FILE>): void {}\n\nfunction main() {\n  const c = new CString(\"x\");\n  console.log(ffi.byteLength(\"ab\"), safe());\n  const g = ffi.readUtf8;\n}\n";
    project(dir.path(), RAW_LIB, NATIVE, main, ALLOW);
    let out = errors(dir.path());
    for raw in [
        "main.vlt:1:19: error: `Ptr` of `velt/ffi` is raw native API",
        "main.vlt:1:24: error: `readCString` of `velt/ffi` is raw native API",
        "main.vlt:3:10: error: `FILE` of `bind` is raw native API",
        "main.vlt:3:16: error: `nothing` of `bind` is raw native API",
        "main.vlt:3:31: error: `Ptr` of `bind` is raw native API",
        "main.vlt:5:19: error: `ffi.Ptr` is raw native API",
        "main.vlt:10:17: error: `ffi.readUtf8` is raw native API",
    ] {
        assert!(out.contains(raw), "{raw}:\n{out}");
    }
    assert!(!out.contains("`CString` of"), "{out}");
    assert!(!out.contains("byteLength"), "{out}");
    assert!(!out.contains("`safe` of"), "{out}");
    assert_eq!(out.matches("is raw native API").count(), 7, "{out}");
}

#[test]
fn unsafe_code_in_a_package_needs_a_safety_comment() {
    let dir = test_dir::TestDir::new();
    let lib = "unsafe function raw(): i64 {\n  return 2;\n}\n\nexport function demo(): void {\n  const x = 1;\n  unsafe {\n    console.log(raw());\n  }\n\n  // SAFETY: a comment a blank line away is not this block's.\n\n  console.log(unsafe { raw() });\n}\n";
    project(dir.path(), lib, NATIVE, DEMO, ALLOW);
    let out = errors(dir.path());
    assert!(
        out.contains(
            "lib.vlt:1:17: error: unsafe function `raw` needs a safety comment directly before it"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "lib.vlt:7:3: error: `unsafe` code needs a `// SAFETY:` comment directly before it"
        ),
        "{out}"
    );
    assert!(
        out.contains("lib.vlt:13:15: error: `unsafe` code needs a `// SAFETY:`"),
        "{out}"
    );
    assert_eq!(out.matches("needs a").count(), 3, "{out}");
}

/// `velt check` lists each allowed package with native code: its C libraries and its unsafe
/// code by file and line (`--json` too). The lockfile records what the program allows.
#[test]
fn velt_check_lists_the_allowed_native_code() {
    let dir = test_dir::TestDir::new();
    let native = ", native: { c: { m: { system: { default: \"m\" } } } }";
    project(dir.path(), UNSAFE_CODE, native, DEMO, ALLOW);
    assert_eq!(
        passes(dir.path(), &["check"]),
        "Native code the program allows (allowNative):\n  \
         bind: C libraries m (m); 6 places of unsafe code\n    \
         src/lib.vlt:2   unsafe function `raw`\n    \
         src/lib.vlt:10  unsafe function `at`\n    \
         src/lib.vlt:18  unsafe block\n    \
         src/lib.vlt:28  unsafe expression\n    \
         src/lib.vlt:30  unsafe block\n    \
         src/lib.vlt:34  unsafe expression\n"
    );
    let json: serde_json::Value =
        serde_json::from_str(&passes(dir.path(), &["check", "--json"])).unwrap();
    assert_eq!(json["native"][0]["package"], "bind");
    assert_eq!(
        json["native"][0]["unsafe"][1]["what"],
        "unsafe function `at`"
    );
    let lock = std::fs::read_to_string(dir.path().join("app/velt.lock.json")).unwrap();
    assert!(
        lock.contains("\"allowNative\": [\n    \"bind\"\n  ]"),
        "{lock}"
    );
    // Nothing is listed for a program without native packages.
    project(
        dir.path(),
        "export function demo(): void {}\n",
        "",
        DEMO,
        "",
    );
    assert_eq!(passes(dir.path(), &["check"]), "");
}
