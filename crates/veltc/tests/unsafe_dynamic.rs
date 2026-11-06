//! What the check on `unsafe` code's place (crates/veltc/src/native/trust.rs) can't see in a
//! module's AST stays out of reach another way (docs/internals/design/c-ffi.md "The trust
//! boundary"): dynamic dispatch, the methods the language calls implicitly, generated code, a
//! pointer's fields through `any`, and npm packages under a package with native code.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A package.velt with `fields` after the name and version.
fn manifest(root: &Path, rel: &str, name: &str, fields: &str) {
    write(
        root,
        &format!("{rel}/package.velt"),
        &format!("import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"{name}\",\n  version: \"1.0.0\",\n  {fields}\n}};\n"),
    );
}

fn velt(cwd: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// `velt check` in `cwd`'s errors (it must fail).
fn errors(cwd: &Path) -> String {
    let o = velt(cwd, &["check"]);
    let out = text(&o);
    assert!(!o.status.success(), "expected errors:\n{out}");
    out
}

/// `good`, allowed, with a class with unsafe members; `evil`, no native code, with source
/// `evil`; `app`, calling `evil`'s `e()`.
fn project(root: &Path, good: &str, evil: &str) {
    manifest(
        root,
        "good",
        "good",
        "native: { c: { c: { unchecked: true } } },",
    );
    write(root, "good/src/lib.velt", good);
    manifest(
        root,
        "evil",
        "evil",
        "dependencies: { good: { path: \"../good\" } },",
    );
    write(root, "evil/src/lib.velt", evil);
    manifest(
        root,
        "app",
        "app",
        "allowNative: [\"good\"],\n  dependencies: { good: { path: \"../good\" }, evil: { path: \"../evil\" } },",
    );
    write(
        root,
        "app/src/main.velt",
        "import { e } from \"evil\";\n\nfunction main() {\n  e();\n}\n",
    );
}

const BUF: &str = r#"import { Ptr, readBytes } from "velt/ffi";

export class Buf {
  /** Unsafe: test. */
  unsafe at(n: i64): i64 {
    console.log("unsafe code ran");
    return n;
  }

  /** Unsafe: `a` is the address of a byte. */
  unsafe peek(a: i64): u8 {
    console.log("unsafe code ran");
    // SAFETY: as the method says.
    return unsafe { readBytes(Ptr.at<u8>(a as usize), 1 as usize)[0] };
  }

  /** Unsafe: test. */
  unsafe get g(): i64 {
    console.log("unsafe code ran");
    return 1;
  }

  /** Unsafe: test. */
  unsafe set s(v: i64) {
    console.log("unsafe code ran");
  }

  plain(n: i64): i64 {
    return n + 1;
  }
}

export class Sub extends Buf {}
"#;

/// The dynamic routes to an unsafe member, each in a function of `evil`.
const ROWS: &[(&str, &str, &str)] = &[
    ("call", "a.at(1)", "at"),
    ("address", "a.peek(0)", "peek"),
    ("computed", "a[\"at\"](1)", "at"),
    ("optional", "a?.at?.(1)", "at"),
    ("getter", "a.g", "g"),
    ("value", "a.at", "at"),
    ("reflect", "Reflect.get(a, \"g\")", "g"),
    ("inherited", "s.at(1)", "at"),
];

/// The same through a proxy, in a program that creates one (other code is generated for it).
const PROXY_ROWS: &[(&str, &str, &str)] = &[
    ("typed", "new Proxy(new Buf(), {}).at(1)", "at"),
    (
        "address",
        "(new Proxy(new Buf() as any, {}) as any).peek(0)",
        "peek",
    ),
    ("getter", "(new Proxy(new Buf() as any, {}) as any).g", "g"),
    ("value", "(new Proxy(new Buf() as any, {}) as any).at", "at"),
];

/// `evil` with a function per row, chosen by the program's first argument.
fn evil_rows(rows: &[(&str, &str, &str)]) -> String {
    let mut s = String::from(
        "import { Buf, Sub } from \"good\";\n\nexport function e() {\n  const a: any = new Buf();\n  const s: any = new Sub();\n  const row = process.argv[process.argv.length - 1];\n  console.log(a.plain(1));\n",
    );
    for (name, code, _) in rows {
        s += &format!("  if (row == \"{name}\") {{\n    console.log({code});\n  }}\n");
    }
    s + "  if (row == \"none\") {\n    console.log(s.plain(2));\n  }\n}\n"
}

/// Builds the program in `mode`, and runs it for each row: the safe control runs, then a
/// `TypeError` naming the member, and neither the unsafe code nor a crash.
fn each_row_is_a_type_error(rows: &[(&str, &str, &str)], mode: &[&str]) {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    project(root, BUF, &evil_rows(rows));
    let app = root.join("app");
    let exe = root.join("program");
    let mut args = vec!["build", "-o", exe.to_str().unwrap()];
    args.extend_from_slice(mode);
    let o = velt(&app, &args);
    assert!(o.status.success(), "{}", text(&o));
    let run = |row: &str| no_window::command(&exe).arg(row).output().unwrap();
    let control = run("none");
    let out = text(&control);
    assert!(control.status.success(), "{out}");
    assert_eq!(String::from_utf8_lossy(&control.stdout), "2\n3\n");
    for (name, code, member) in rows {
        let o = run(name);
        let out = text(&o);
        assert!(!out.contains("unsafe code ran"), "{code}: {out}");
        assert!(
            out.contains("TypeError") && out.contains(&format!("'{member}'")),
            "{code}: {out}"
        );
        // A panic's exit, not a signal.
        assert!(o.status.code().is_some(), "{code}: {:?}", o.status);
        assert!(!o.status.success(), "{code}: {out}");
    }
}

#[test]
fn any_doesn_t_reach_unsafe_members() {
    each_row_is_a_type_error(ROWS, &[]);
}

#[test]
fn any_doesn_t_reach_unsafe_members_in_release_builds() {
    each_row_is_a_type_error(ROWS, &["--release"]);
}

#[test]
fn a_proxy_doesn_t_reach_unsafe_members() {
    each_row_is_a_type_error(PROXY_ROWS, &[]);
}

#[test]
fn a_proxy_doesn_t_reach_unsafe_members_in_release_builds() {
    each_row_is_a_type_error(PROXY_ROWS, &["--release"]);
}

/// A write through `any` to an unsafe setter, and an interface over an `any`, stop too.
#[test]
fn a_write_and_an_interface_view_don_t_reach_unsafe_members() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let write_row = "import { Buf } from \"good\";\n\nexport function e() {\n  const a: any = new Buf();\n  a.s = 1;\n  console.log(\"done\");\n}\n";
    project(root, BUF, write_row);
    let o = velt(&root.join("app"), &["run"]);
    let out = text(&o);
    assert!(!out.contains("unsafe code ran"), "{out}");
    assert!(out.contains("TypeError") && out.contains("'s'"), "{out}");
    let view = "import { Buf } from \"good\";\n\ninterface Atter {\n  at(n: i64): i64;\n}\n\nexport function e() {\n  const a: any = new Buf();\n  const i = a as Atter;\n  console.log(i.at(1));\n}\n";
    project(root, BUF, view);
    let o = velt(&root.join("app"), &["run"]);
    let out = text(&o);
    assert!(!out.contains("unsafe code ran"), "{out}");
    assert!(!o.status.success(), "{out}");
    assert!(o.status.code().is_some(), "{:?}", o.status);
}

/// The methods the language calls by itself can't be unsafe, so `String(a)`, `${a}`, `a + 1`,
/// `JSON.stringify(a)` and `await a` have nothing to reach, through `any` or not.
#[test]
fn what_the_language_calls_implicitly_can_t_be_unsafe() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    for method in [
        "unsafe toString(): string { return \"\"; }",
        "unsafe toLocaleString(): string { return \"\"; }",
        "unsafe valueOf(): i64 { return 1; }",
        "unsafe toJSON(): string { return \"\"; }",
        "unsafe then(): void {}",
        "unsafe get toString(): string { return \"\"; }",
    ] {
        let good = format!("export class C {{\n  /** Unsafe: test. */\n  {method}\n}}\n");
        project(
            root,
            &good,
            "import { C } from \"good\";\n\nexport function e() {\n  new C();\n}\n",
        );
        let out = errors(&root.join("app"));
        assert!(out.contains("can't be unsafe"), "{method}: {out}");
    }
}

const GENERATOR: &str = r#"import { InstanceRequest, Output } from "velt/generate";

export function boom(req: InstanceRequest, out: Output): void {
  out.line(`return unsafe { ${req.params[0]}.peek(0) } as i64;`);
}
"#;

/// A generator of a package with no native code writes unsafe code; of a program with no
/// packages too.
#[test]
fn generated_code_can_t_contain_unsafe_code() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    project(
        root,
        BUF,
        "import { Buf } from \"good\";\n\nexport declare generated(\"./gen\") function boom<T>(b: Buf): i64;\n\nexport function e() {\n  console.log(boom<i64>(new Buf()));\n}\n",
    );
    write(root, "evil/src/gen.velt", GENERATOR);
    let out = errors(&root.join("app"));
    assert!(
        out.contains("generated code cannot contain `unsafe` code"),
        "{out}"
    );

    let lone = root.join("lone");
    write(
        &lone,
        "main.velt",
        "declare generated(\"./gen\") function x<T>(): i64;\n\nfunction main() {\n  console.log(x<i64>());\n}\n",
    );
    write(
        &lone,
        "gen.velt",
        "import { InstanceRequest, Output } from \"velt/generate\";\n\nexport function x(req: InstanceRequest, out: Output): void {\n  out.line(\"const y = unsafe { 1 };\");\n  out.line(\"return y;\");\n}\n",
    );
    let o = velt(&lone, &["check", "main.velt"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.contains("generated code cannot contain `unsafe` code"),
        "{out}"
    );
}

const POINTERS: &str = r#"import { CString, Out, OutPtr, Ptr } from "velt/ffi";

declare c("c") function strlen(s: Ptr<u8>): usize;
declare c("c") function calloc(n: usize, size: usize): Ptr<u8>;
declare c("c") function strcpy(dst: Ptr<u8>, src: CString): Ptr<u8>;
declare c("c") function strtoll(s: CString, end: OutPtr<u8>, base: i32): i64;
declare c("c") function strnlen(s: CString, n: usize): usize;

export function getp(): Ptr<u8> {
  // SAFETY: a copy of a NUL-terminated string, never freed.
  return unsafe { strcpy(calloc(3 as usize, 1 as usize), new CString("hi")) };
}

export function deref(p: Ptr<u8>): usize {
  // SAFETY: a `Ptr` holds what `getp` returned.
  return unsafe { strlen(p) };
}

export function out(): OutPtr<u8> {
  const o = new OutPtr<u8>();
  // SAFETY: `o` is an out parameter, the string is NUL-terminated.
  unsafe { strtoll(new CString("12"), o, 10); }
  return o;
}

export function derefOut(o: OutPtr<u8>): usize {
  const p = o.value;
  return p === null ? 0 as usize : Ptr.addressOf(p) == 16 ? 16 as usize : 1 as usize;
}

export function cLen(c: CString): usize {
  // SAFETY: `c` is NUL-terminated.
  return unsafe { strnlen(c, 100 as usize) };
}
"#;

/// Each write to a pointer's address through `any`, from safe code, is refused before the
/// address reaches C; the plain calls still work (an `OutPtr` is written by C, not by Velt).
#[test]
fn a_pointer_s_address_can_t_be_written_through_any() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let app = root.join("app");
    let wrap = |body: &str| {
        format!(
            "import {{ cLen, deref, derefOut, getp, out }} from \"good\";\nimport {{ CString }} from \"velt/ffi\";\n\nexport function e() {{\n{body}\n}}\n"
        )
    };
    project(
        root,
        POINTERS,
        &wrap("  console.log(deref(getp()), derefOut(out()), cLen(new CString(\"abc\")));"),
    );
    for mode in [&[][..], &["--release"][..]] {
        let mut args = vec!["run"];
        args.extend_from_slice(mode);
        let o = velt(&app, &args);
        assert!(o.status.success(), "{}", text(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "2 1 3\n");
    }
    // `Ptr` is a struct: its field is `readonly`.
    let p = "  const p: any = getp();\n  p.address = 16;\n  console.log(deref(p));";
    project(root, POINTERS, &wrap(p));
    let o = velt(&app, &["run"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(o.status.code().is_some(), "{:?}", o.status);
    assert!(
        out.contains("TypeError") && out.contains("'address'"),
        "{out}"
    );
    // `OutPtr` and `CString` are classes of std: their private fields are no members through
    // `any`, so C still gets what they held.
    for (body, held) in [
        ("  const p: any = out();\n  p.address = 16;\n  console.log(derefOut(p));", "1\n"),
        ("  const c: any = new CString(\"abc\");\n  c.bytes = [1 as u8];\n  console.log(cLen(c));", "3\n"),
    ] {
        project(root, POINTERS, &wrap(body));
        let o = velt(&app, &["run"]);
        let out = text(&o);
        assert!(o.status.code().is_some(), "{body}: {:?}", o.status);
        assert!(
            !o.status.success() || String::from_utf8_lossy(&o.stdout) == held,
            "{body}: {out}"
        );
    }
}

/// `good` re-exports `Ptr` through an import cycle (`lib` ↔ `b`): whichever module `evil` reads
/// first, both are raw.
#[test]
fn raw_exports_go_around_an_import_cycle() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let good = "export { Ptr } from \"velt/ffi\";\nexport * from \"./b\";\nexport function f(): i32 {\n  return 1;\n}\n";
    for evil in [
        "import { f } from \"good\";\nimport { Ptr } from \"good/b\";\n\nexport function e() {\n  console.log(f());\n}\n",
        "import { Ptr } from \"good/b\";\n\nexport function e() {}\n",
        "import { Ptr } from \"good\";\n\nexport function e() {}\n",
    ] {
        project(root, good, evil);
        write(root, "good/src/b.velt", "export * from \"./lib\";\n");
        let out = errors(&root.join("app"));
        assert!(out.contains("`Ptr` of `good"), "{evil}: {out}");
        assert!(out.contains("is raw native API"), "{evil}: {out}");
    }
}

/// A C function a package exports is raw: sema calls it unsafe.
#[test]
fn an_exported_c_function_is_raw() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    project(
        root,
        "export declare c(\"c\") function getpid(): i32;\n",
        "import { getpid } from \"good\";\n\nexport function e() {}\n",
    );
    let out = errors(&root.join("app"));
    assert!(
        out.contains("`getpid` of `good` is raw native API"),
        "{out}"
    );
}

/// The three npm packages under a package with native code: `unsafe` code in them is an error,
/// as is the raw API, as they have no package's trust. Each has what the others have.
#[test]
fn npm_packages_have_no_package_s_trust() {
    let npm = "import { Ptr } from \"velt/ffi\";\n\nexport function v(): i64 {\n  return unsafe { 1 };\n}\n";
    let package_json = "{ \"name\": \"x\", \"version\": \"1.0.0\", \"main\": \"src/index.ts\" }";
    let call = "import { v } from \"x\";\n\nfunction main() {\n  console.log(v());\n}\n";
    let check = |root: &Path, what: &str| {
        let out = errors(&root.join("app"));
        assert!(
            out.contains("`unsafe` code needs a native package"),
            "{what}: {out}"
        );
        assert!(
            out.contains("`Ptr` of `velt/ffi` is raw native API"),
            "{what}: {out}"
        );
    };

    // Under an allowed dependency.
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(root, "good", "good", "native: { c: {} },");
    write(root, "good/src/lib.velt", "export { v } from \"x\";\n");
    write(root, "good/node_modules/x/package.json", package_json);
    write(root, "good/node_modules/x/src/index.ts", npm);
    manifest(
        root,
        "app",
        "app",
        "allowNative: [\"good\"],\n  dependencies: { good: { path: \"../good\" } },",
    );
    write(root, "app/src/main.velt", &call.replace("\"x\"", "\"good\""));
    check(root, "good/node_modules/x");

    // Under the root, which has native code.
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(root, "app", "app", "native: { c: {} },");
    write(root, "app/src/main.velt", call);
    write(root, "app/node_modules/x/package.json", package_json);
    write(root, "app/node_modules/x/src/index.ts", npm);
    check(root, "app/node_modules/x");

    // In the repository of an allowed git dependency, next to it.
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    manifest(root, "repo", "good", "native: { c: {} },");
    write(root, "repo/src/lib.velt", "export const n = 1;\n");
    write(root, "repo/js/package.json", package_json);
    write(root, "repo/js/src/index.ts", npm);
    let repo: PathBuf = root.join("repo");
    for args in [
        &["init", "-q", "."][..],
        &["add", "-A"],
        &[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=a",
            "commit",
            "-q",
            "-m",
            "x",
        ],
    ] {
        let o = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {}", text(&o));
    }
    let path = repo.canonicalize().unwrap();
    // On Windows, `canonicalize` gives a verbatim `//?/C:/...` path, whose `?` a URL can't hold.
    let path = path.to_string_lossy().replace('\\', "/");
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    let url = match path.starts_with('/') {
        true => format!("file://{path}"),
        false => format!("file:///{path}"),
    };
    manifest(
        root,
        "app",
        "app",
        &format!(
            "allowNative: [{{ name: \"good\", git: \"{url}\" }}],\n  dependencies: {{ good: {{ git: \"{url}\" }}, x: {{ git: \"{url}\", path: \"js\" }} }},"
        ),
    );
    write(root, "app/src/main.velt", call);
    check(root, "repo/js");
}

/// `export { … }` may come before the import it names: a raw name is raw in either order, under
/// its own name or another.
#[test]
fn a_local_export_before_its_import_is_raw() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let import = "import { Ptr as Q } from \"velt/ffi\";\n";
    for (good, name) in [
        (format!("export {{ Q }};\n{import}"), "Q"),
        (format!("export {{ Q as R }};\n{import}"), "R"),
        (format!("{import}export {{ Q }};\n"), "Q"),
        (format!("{import}export {{ Q as R }};\n"), "R"),
    ] {
        let evil = format!("import {{ {name} }} from \"good\";\n\nexport function e() {{}}\n");
        project(root, &good, &evil);
        let out = errors(&root.join("app"));
        assert!(
            out.contains(&format!("`{name}` of `good` is raw native API")),
            "{good}: {out}"
        );
    }
}

/// A getter and a setter of one name, one of them unsafe: through `any` the safe one works and
/// the unsafe one is the `TypeError`, whichever way round.
#[test]
fn an_unsafe_accessor_leaves_its_safe_other_half_alone() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    let app = root.join("app");
    let class = |members: &str| format!("export class C {{\n  n: i64 = 1;\n{members}\n}}\n");
    let unsafe_setter = class(
        "  get x(): i64 {\n    return this.n;\n  }\n\n  /** Unsafe: test. */\n  unsafe set x(v: i64) {\n    console.log(\"unsafe code ran\");\n    this.n = v;\n  }",
    );
    let unsafe_getter = class(
        "  /** Unsafe: test. */\n  unsafe get x(): i64 {\n    console.log(\"unsafe code ran\");\n    return this.n;\n  }\n\n  set x(v: i64) {\n    this.n = v;\n  }",
    );
    let evil = |body: &str| {
        format!("import {{ C }} from \"good\";\n\nexport function e() {{\n  const a: any = new C();\n{body}\n}}\n")
    };
    let read = "  console.log(a.x);";
    let write = "  a.x = 2;\n  console.log(a.n);";
    let works = |good: &str, body: &str, expected: &str| {
        project(root, good, &evil(body));
        let o = velt(&app, &["run"]);
        assert!(o.status.success(), "{good}{body}: {}", text(&o));
        assert_eq!(String::from_utf8_lossy(&o.stdout), expected, "{good}{body}");
    };
    let stops = |good: &str, body: &str| {
        project(root, good, &evil(body));
        let o = velt(&app, &["run"]);
        let out = text(&o);
        assert!(!o.status.success(), "{good}{body}: {out}");
        assert!(!out.contains("unsafe code ran"), "{good}{body}: {out}");
        assert!(
            out.contains("TypeError: 'x' is unsafe"),
            "{good}{body}: {out}"
        );
    };
    works(&unsafe_setter, read, "1\n");
    stops(&unsafe_setter, write);
    works(&unsafe_getter, write, "2\n");
    stops(&unsafe_getter, read);
}
