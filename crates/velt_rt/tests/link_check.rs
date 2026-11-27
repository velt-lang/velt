//! End-to-end check that the *staticlib* links into a real executable with the platform linker,
//! exactly the way compiled Velt programs are linked: a foreign object that only defines
//! `velt_main` + `velt_rt.lib`/`libvelt_rt.a` + the native libs from NATIVE_LIBS.md, with the C
//! `main` coming from the runtime.
//!
//! The "generated code" object is compiled from C (cl.exe on MSVC, `cc` elsewhere) with no default
//! library directives (`/Zl`), mirroring what Cranelift emits. This test deliberately does not
//! `use velt_rt`: linking the non-test rlib would bring in a second `main`.
//!
//! If no C compiler is found, the test prints a note and passes (it cannot prove anything then).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PRELUDE: &str = r#"
#include <stdint.h>
typedef struct { uint8_t* ptr; uint64_t len; uint64_t cap; } VeltStr;
void velt_rt_write_str(uint32_t, const VeltStr*);
void velt_rt_write_bool(uint32_t, uint8_t);
void velt_rt_write_byte(uint32_t, uint8_t);
void velt_rt_flush(void);
void velt_rt_str_concat(const VeltStr*, const VeltStr*, VeltStr*);
void velt_rt_str_append(VeltStr*, const VeltStr*);
void velt_rt_str_drop(VeltStr*);
int32_t velt_rt_str_cmp(const VeltStr*, const VeltStr*);
void* velt_rt_alloc(uint64_t, uint64_t);
void velt_rt_free(void*, uint64_t, uint64_t);
void velt_rt_panic(const VeltStr*);
void velt_rt_exit(int32_t);
// An ASCII literal: `w1` packs the UTF-16 length (high half) and the byte length (low half).
#define LIT(s) { (uint8_t*)(s), ((uint64_t)(sizeof(s) - 1) << 32) | (sizeof(s) - 1), 0 }
// An integer's decimal digits through `velt_rt_write_str`, as std's `intWrite` writes them (the
// runtime prints no integer, #2197).
static inline void write_u64(uint32_t stream, uint64_t v) {
    char b[20]; int n = 20;
    do { b[--n] = (char)('0' + v % 10); v /= 10; } while (v);
    VeltStr t = { (uint8_t*)b + n, ((uint64_t)(20 - n) << 32) | (uint64_t)(20 - n), 0 };
    velt_rt_write_str(stream, &t);
}
static inline void write_i64(uint32_t stream, int64_t v) {
    if (v < 0) { VeltStr m = LIT("-"); velt_rt_write_str(stream, &m); }
    write_u64(stream, v < 0 ? 0 - (uint64_t)v : (uint64_t)v);
}
"#;

const HELLO: &str = r#"
int32_t velt_main(void) {
    // Floats' text is std's (velt/lang/number/format): the line keeps them as literals.
    VeltStr a = LIT("Hello, "), b = LIT("world"), c, f = LIT("0.30000000000000004 1e+21 ");
    velt_rt_str_concat(&a, &b, &c);
    velt_rt_str_append(&c, &b);
    velt_rt_write_str(1, &c); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&c);
    write_i64(1, -42); velt_rt_write_byte(1, ' ');
    write_u64(1, 18446744073709551615ull); velt_rt_write_byte(1, ' ');
    velt_rt_write_str(1, &f);
    velt_rt_write_bool(1, 1); velt_rt_write_byte(1, '\n');
    VeltStr g = LIT("1.5e-7");
    velt_rt_write_str(1, &g); velt_rt_write_byte(1, '\n');
    void* p = velt_rt_alloc(1024, 16);
    velt_rt_free(p, 1024, 16);
    VeltStr e = LIT("to stderr");
    velt_rt_write_str(2, &e); velt_rt_write_byte(2, '\n');
    return 3;
}
"#;

const PANIC: &str = r#"
int32_t velt_main(void) {
    VeltStr s = LIT("before"), m = LIT("division by zero");
    velt_rt_write_str(1, &s); velt_rt_write_byte(1, '\n');
    velt_rt_panic(&m);
    return 0;
}
"#;

const EXIT: &str = r#"
int32_t velt_main(void) {
    int i;
    for (i = 0; i < 20000; i++) { write_i64(1, i); velt_rt_write_byte(1, '\n'); }
    velt_rt_exit(7);
    return 0;
}
"#;

/// A worker of the executor in Velt starts with `velt_rt_set_scheduler(2)`, which links and does
/// nothing: the runtime has no host of its own any more.
const HOST: &str = r#"
void velt_rt_set_scheduler(uint32_t);
int32_t velt_main(void) {
    velt_rt_set_scheduler(2);
    write_i64(1, 1); velt_rt_write_byte(1, '\n');
    return 5;
}
"#;

fn target_dir() -> PathBuf {
    // <target>/<profile>/deps/link_check-<hash>.exe
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().to_path_buf()
}

fn runtime_lib() -> PathBuf {
    let name = if cfg!(target_env = "msvc") {
        "velt_rt.lib"
    } else {
        "libvelt_rt.a"
    };
    let p = target_dir().join(name);
    assert!(
        p.exists(),
        "staticlib not found at {} (build it first: `cargo build -p velt_rt`, with `--release` for a release test run)",
        p.display()
    );
    p
}

fn work_dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("velt_rt_link_check")
        .join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_ok(mut cmd: Command, what: &str) {
    let out = cmd.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(target_env = "msvc")]
const TARGET: &str = if cfg!(target_arch = "aarch64") {
    "aarch64-pc-windows-msvc"
} else {
    "x86_64-pc-windows-msvc"
};

/// Build an executable from C source `body`; returns None if no toolchain is available.
#[cfg(target_env = "msvc")]
fn build(name: &str, body: &str) -> Option<PathBuf> {
    let (Some(mut cl), Some(mut link)) = (
        cc::windows_registry::find(TARGET, "cl.exe"),
        cc::windows_registry::find(TARGET, "link.exe"),
    ) else {
        return None;
    };
    let dir = work_dir(name);
    let src = dir.join("main.c");
    std::fs::write(&src, format!("{PRELUDE}{body}")).unwrap();
    let obj = dir.join("main.obj");
    let exe = dir.join(format!("{name}.exe"));
    // /Zl: no default-library directives in the object (like a Cranelift object); /GS-: no
    // security-cookie dependency.
    cl.args(["/nologo", "/c", "/O1", "/Zl", "/GS-"])
        .arg(format!("/Fo{}", obj.display()))
        .arg(&src);
    run_ok(cl, "cl.exe");
    // Exactly the command line documented in NATIVE_LIBS.md for the tooling agent.
    link.args(["/NOLOGO", "/SUBSYSTEM:CONSOLE"])
        .arg(format!("/OUT:{}", exe.display()))
        .arg(&obj)
        .arg(runtime_lib())
        .args(NATIVE_LIBS);
    run_ok(link, "link.exe");
    Some(exe)
}

#[cfg(target_env = "msvc")]
const NATIVE_LIBS: &[&str] = &[
    "psapi.lib",
    "shell32.lib",
    "user32.lib",
    "advapi32.lib",
    "bcrypt.lib",
    "kernel32.lib",
    "ntdll.lib",
    "userenv.lib",
    "ws2_32.lib",
    "dbghelp.lib",
    "secur32.lib",
    "msvcrt.lib",
];

#[cfg(not(target_env = "msvc"))]
fn build(name: &str, body: &str) -> Option<PathBuf> {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    if command(&cc).arg("--version").output().is_err() {
        return None;
    }
    let dir = work_dir(name);
    let src = dir.join("main.c");
    std::fs::write(&src, format!("{PRELUDE}{body}")).unwrap();
    let obj = dir.join("main.o");
    let exe = dir.join(name);
    // macOS `cc` builds for the arch the calling process runs as; pin it to this test's arch
    // (the runtime library's) so an x86_64 test run under Rosetta links x86_64.
    let arch: &[&str] = match std::env::consts::ARCH {
        "x86_64" if cfg!(target_os = "macos") => &["-arch", "x86_64"],
        "aarch64" if cfg!(target_os = "macos") => &["-arch", "arm64"],
        _ => &[],
    };
    let mut c = command(&cc);
    c.args(arch).args(["-c", "-O1", "-o"]).arg(&obj).arg(&src);
    run_ok(c, "cc -c");
    let mut l = command(&cc);
    l.args(arch)
        .arg("-o")
        .arg(&exe)
        .arg(&obj)
        .arg(runtime_lib());
    if cfg!(target_os = "macos") {
        l.args([
            "-framework",
            "SystemConfiguration",
            "-framework",
            "CoreFoundation",
            "-lSystem",
            "-lc",
            "-lm",
            "-liconv",
        ]);
    } else {
        l.args([
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]);
    }
    run_ok(l, "cc (link)");
    Some(exe)
}

fn run(exe: &Path) -> Output {
    command(exe).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

#[test]
fn staticlib_links_and_runs() {
    let Some(exe) = build("hello", HELLO) else {
        eprintln!("NOTE: no C toolchain found; link check skipped");
        return;
    };
    let out = run(&exe);
    assert_eq!(
        text(&out.stdout),
        "Hello, worldworld\n-42 18446744073709551615 0.30000000000000004 1e+21 true\n1.5e-7\n"
    );
    assert_eq!(text(&out.stderr), "to stderr\n");
    assert_eq!(out.status.code(), Some(3));

    let exe = build("panic", PANIC).unwrap();
    let out = run(&exe);
    assert_eq!(text(&out.stdout), "before\n");
    assert_eq!(text(&out.stderr), "panic: division by zero\n");
    assert_eq!(out.status.code(), Some(101));

    let exe = build("exit", EXIT).unwrap();
    let out = run(&exe);
    let expected: String = (0..20000).map(|i| format!("{i}\n")).collect();
    assert_eq!(text(&out.stdout), expected);
    assert_eq!(out.status.code(), Some(7));

    // A worker's first call links and changes nothing.
    let exe = build("host", HOST).unwrap();
    let out = run(&exe);
    assert_eq!(text(&out.stdout), "1\n");
    assert_eq!(out.status.code(), Some(5));

    let exe = build("json", JSON).unwrap();
    let out = run(&exe);
    assert_eq!(text(&out.stdout), "{\"name\":\"ann\",\"age\":30}\n");
}

/// The string builder from C, building a JSON object's text as generated code does.
const JSON: &str = r#"
void velt_rt_strbuf_new(uint64_t, VeltStr*);
void velt_rt_strbuf_push_str(VeltStr*, const VeltStr*);
void velt_rt_strbuf_push_byte(VeltStr*, uint8_t);
void velt_rt_strbuf_finish(VeltStr*, VeltStr*);
int32_t velt_main(void) {
    VeltStr b, s, name = LIT("\"ann\""), p1 = LIT("{\"name\":"), p2 = LIT(",\"age\":");
    velt_rt_strbuf_new(0, &b);
    velt_rt_strbuf_push_str(&b, &p1);
    velt_rt_strbuf_push_str(&b, &name);
    velt_rt_strbuf_push_str(&b, &p2);
    velt_rt_strbuf_push_byte(&b, '3'); velt_rt_strbuf_push_byte(&b, '0');
    velt_rt_strbuf_push_byte(&b, '}');
    velt_rt_strbuf_finish(&b, &s);
    velt_rt_write_str(1, &s); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&s);
    return 0;
}
"#;

#[path = "../../../tests/common/command.rs"]
mod command;
use command::command;
