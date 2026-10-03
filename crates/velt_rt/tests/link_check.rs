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
use std::time::Duration;

const PRELUDE: &str = r#"
#include <stdint.h>
typedef struct { uint8_t* ptr; uint64_t len; uint64_t cap; } VeltStr;
void velt_rt_write_str(uint32_t, const VeltStr*);
void velt_rt_write_i64(uint32_t, int64_t);
void velt_rt_write_u64(uint32_t, uint64_t);
void velt_rt_write_f64(uint32_t, double);
void velt_rt_write_bool(uint32_t, uint8_t);
void velt_rt_write_byte(uint32_t, uint8_t);
void velt_rt_flush(void);
void velt_rt_str_concat(const VeltStr*, const VeltStr*, VeltStr*);
void velt_rt_str_append(VeltStr*, const VeltStr*);
void velt_rt_str_from_f64(double, VeltStr*);
void velt_rt_str_drop(VeltStr*);
int32_t velt_rt_str_cmp(const VeltStr*, const VeltStr*);
void* velt_rt_alloc(uint64_t, uint64_t);
void velt_rt_free(void*, uint64_t, uint64_t);
void velt_rt_panic(const VeltStr*);
void velt_rt_exit(int32_t);
int64_t velt_rt_pow_i64(int64_t, int64_t);
double velt_rt_pow_f64(double, double);
#define LIT(s) { (uint8_t*)(s), sizeof(s) - 1, 0 }
typedef struct VeltFut { uint32_t (*poll)(struct VeltFut*, void*); void (*drop)(struct VeltFut*); } VeltFut;
typedef uint32_t (*PollFn)(void*, void*);
typedef void (*DropFn)(void*);
VeltFut* velt_rt_sleep(int64_t);
uint32_t velt_rt_fut_poll(VeltFut*, void*);
void velt_rt_fut_drop(VeltFut*);
VeltFut* velt_rt_spawn(PollFn, DropFn, const void*, uint64_t, uint64_t, uint64_t, void (*)(void*));
void velt_rt_block_on(PollFn, void*);
"#;

const HELLO: &str = r#"
int32_t velt_main(void) {
    VeltStr a = LIT("Hello, "), b = LIT("world"), c, f;
    velt_rt_str_concat(&a, &b, &c);
    velt_rt_str_append(&c, &b);
    velt_rt_write_str(1, &c); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&c);
    velt_rt_write_i64(1, -42); velt_rt_write_byte(1, ' ');
    velt_rt_write_u64(1, 18446744073709551615ull); velt_rt_write_byte(1, ' ');
    velt_rt_write_f64(1, 0.1 + 0.2); velt_rt_write_byte(1, ' ');
    velt_rt_write_f64(1, velt_rt_pow_f64(10.0, 21.0)); velt_rt_write_byte(1, ' ');
    velt_rt_write_bool(1, 1); velt_rt_write_byte(1, ' ');
    velt_rt_write_i64(1, velt_rt_pow_i64(2, 62)); velt_rt_write_byte(1, '\n');
    velt_rt_str_from_f64(1.5e-7, &f);
    velt_rt_write_str(1, &f); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&f);
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
    for (i = 0; i < 20000; i++) { velt_rt_write_i64(1, i); velt_rt_write_byte(1, '\n'); }
    velt_rt_exit(7);
    return 0;
}
"#;

/// `async function square(x) { return x * x; }` spawned from
/// `async function main() { console.log(1); await sleep(10); console.log(await spawn(square(7))); return 5; }`
/// (the two lines may be printed from different workers: their order must still hold).
const ASYNC: &str = r#"
typedef struct { int64_t result; int64_t x; } Square;
static uint32_t square_poll(void* s, void* cx) { Square* q = s; q->result = q->x * q->x; return 1; }
static void square_drop(void* s) { (void)s; }
typedef struct { int32_t result; uint32_t tag; VeltFut* f; } Main;
static uint32_t main_poll(void* s, void* cx) {
    Main* m = s;
    for (;;) switch (m->tag) {
    case 0: velt_rt_write_i64(1, 1); velt_rt_write_byte(1, 10); m->f = velt_rt_sleep(10); m->tag = 1; break;
    case 1: {
        Square q = { 0, 7 };
        if (!velt_rt_fut_poll(m->f, cx)) return 0;
        velt_rt_fut_drop(m->f);
        m->f = velt_rt_spawn(square_poll, square_drop, &q, sizeof q, 8, 8, 0);
        m->tag = 2;
        break;
    }
    default: {
        int64_t r;
        if (!velt_rt_fut_poll(m->f, cx)) return 0;
        r = *(int64_t*)((char*)m->f + 16);
        velt_rt_fut_drop(m->f);
        velt_rt_write_i64(1, r); velt_rt_write_byte(1, '\n');
        m->result = 5;
        return 1;
    }
    }
}
int32_t velt_main(void) { Main m = { 0, 0, 0 }; velt_rt_block_on(main_poll, &m); return m.result; }
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
    assert!(p.exists(), "staticlib not found at {}", p.display());
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
        "Hello, worldworld\n-42 18446744073709551615 0.30000000000000004 1e+21 true 4611686018427387904\n1.5e-7\n"
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

    // The async runtime (tokio) links with the same native libs.
    let exe = build("async", ASYNC).unwrap();
    let out = run(&exe);
    assert_eq!(text(&out.stdout), "1\n49\n");
    assert_eq!(out.status.code(), Some(5));

    let exe = build("json", JSON).unwrap();
    let out = run(&exe);
    assert_eq!(
        text(&out.stdout),
        "{\"name\":\"ann\",\"age\":30}\nexpected string at $.name\n2\n{\"a\":[1,2.5,{\"b\":null}],\"c\":true}\n"
    );
}

/// String builder, JSON reader error path and `json.Value` from C, like generated code for
/// tests/golden/m4/json.vlt.
const JSON: &str = r#"
typedef struct Reader Reader;
void velt_rt_strbuf_new(uint64_t, VeltStr*);
void velt_rt_strbuf_push_str(VeltStr*, const VeltStr*);
void velt_rt_strbuf_push_i64(VeltStr*, int64_t);
void velt_rt_strbuf_push_json_str(VeltStr*, const VeltStr*);
void velt_rt_strbuf_push_byte(VeltStr*, uint8_t);
void velt_rt_strbuf_finish(VeltStr*, VeltStr*);
uint8_t velt_rt_str_eq(const VeltStr*, const VeltStr*);
Reader* velt_rt_json_reader_new(const VeltStr*);
void velt_rt_json_reader_free(Reader*);
uint8_t velt_rt_json_reader_expect_object_start(Reader*);
uint8_t velt_rt_json_reader_next_key(Reader*, VeltStr*);
uint8_t velt_rt_json_reader_read_string(Reader*, VeltStr*);
void velt_rt_json_error(const Reader*, const VeltStr*, const VeltStr*, VeltStr*);
uint8_t velt_rt_json_parse_value(const VeltStr*, uint64_t*, VeltStr*);
uint64_t velt_rt_json_value_get(uint64_t, const VeltStr*);
uint32_t velt_rt_json_value_kind(uint64_t);
void velt_rt_json_value_stringify(uint64_t, VeltStr*);
void velt_rt_json_value_free(uint64_t);
int32_t velt_main(void) {
    VeltStr b, s, k, e, name = LIT("ann"), p1 = LIT("{\"name\":"), p2 = LIT(",\"age\":");
    VeltStr doc = LIT("{\"name\": 1}"), want = LIT("name"), ex = LIT("string"), path = LIT("$.name");
    VeltStr src = LIT("{\"a\":[1,2.5,{\"b\":null}],\"c\":true}"), c = LIT("c"), err;
    uint64_t v, cv;
    Reader* r;
    velt_rt_strbuf_new(0, &b);
    velt_rt_strbuf_push_str(&b, &p1);
    velt_rt_strbuf_push_json_str(&b, &name);
    velt_rt_strbuf_push_str(&b, &p2);
    velt_rt_strbuf_push_i64(&b, 30);
    velt_rt_strbuf_push_byte(&b, '}');
    velt_rt_strbuf_finish(&b, &s);
    velt_rt_write_str(1, &s); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&s);
    r = velt_rt_json_reader_new(&doc);
    if (velt_rt_json_reader_expect_object_start(r) && velt_rt_json_reader_next_key(r, &k) == 1
        && velt_rt_str_eq(&k, &want) && !velt_rt_json_reader_read_string(r, &s)) {
        velt_rt_json_error(r, &ex, &path, &e);
        velt_rt_write_str(1, &e); velt_rt_write_byte(1, '\n');
        velt_rt_str_drop(&e);
    }
    velt_rt_str_drop(&k);
    velt_rt_json_reader_free(r);
    if (!velt_rt_json_parse_value(&src, &v, &err)) return 1;
    cv = velt_rt_json_value_get(v, &c);
    velt_rt_write_i64(1, velt_rt_json_value_kind(cv)); velt_rt_write_byte(1, '\n');
    velt_rt_json_value_free(cv);
    velt_rt_json_value_stringify(v, &s);
    velt_rt_json_value_free(v);
    velt_rt_write_str(1, &s); velt_rt_write_byte(1, '\n');
    velt_rt_str_drop(&s);
    return 0;
}
"#;

/// `async function main() { console.log("ready"); await sleep(86_400_000); }` — like a server
/// that logs and then waits for connections, for a day.
const IDLE: &str = r#"
typedef struct { int32_t result; uint32_t tag; VeltFut* f; } Idle;
static uint32_t idle_poll(void* s, void* cx) {
    Idle* m = s;
    if (m->tag == 0) {
        VeltStr r = LIT("ready");
        velt_rt_write_str(1, &r); velt_rt_write_byte(1, 10);
        m->f = velt_rt_sleep(86400000);
        m->tag = 1;
    }
    if (!velt_rt_fut_poll(m->f, cx)) return 0;
    velt_rt_fut_drop(m->f);
    return 1;
}
int32_t velt_main(void) { Idle m = { 0, 0, 0 }; velt_rt_block_on(idle_poll, &m); return 0; }
"#;

/// Kills the child when dropped, so a failing assertion does not leave it sleeping.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Output written before the workers go idle reaches a pipe while the program is still running,
/// not when it exits. The program sleeps for a day after printing, so the line can only arrive
/// through the idle flush: no deadline on how soon (a loaded machine may take seconds just to
/// start the process), only a hang guard in case it never does.
#[test]
fn piped_output_is_flushed_when_workers_idle() {
    use std::io::BufRead;
    let Some(exe) = build("idle", IDLE) else {
        eprintln!("NOTE: no C toolchain found; idle flush check skipped");
        return;
    };
    let mut child = KillOnDrop(
        command(&exe)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    let line = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("no output after 120 s: buffered output is not flushed when the workers idle")
        .unwrap();
    assert_eq!(line.trim_end(), "ready");
    // The line arrived while the program sleeps: it was flushed at idle, not at exit.
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "the program exited before its output arrived"
    );
}

/// 64 tasks each print 1000 lines built from several `write_*` calls (like one `console.log` with
/// several arguments). Even tasks yield every 100 lines (migrating between workers), odd ones print
/// everything in one poll (crossing the buffer size mid-line).
const STDOUT_STRESS: &str = r#"
void velt_rt_yield_now(void*);
static char PAD[128];
typedef struct { int32_t result; uint32_t tag; int64_t id, line; } Printer;
static uint32_t printer_poll(void* s, void* cx) {
    Printer* p = s;
    while (p->line < 1000) {
        VeltStr t = LIT("t="), l = LIT(" l="), sp = LIT(" ");
        int64_t n = (p->id * 7 + p->line * 13) % 120;
        VeltStr pad = { (uint8_t*)PAD, (uint64_t)n, 0 };
        velt_rt_write_str(1, &t); velt_rt_write_i64(1, p->id);
        velt_rt_write_str(1, &l); velt_rt_write_i64(1, p->line);
        velt_rt_write_str(1, &sp); velt_rt_write_str(1, &pad);
        velt_rt_write_str(1, &sp); velt_rt_write_i64(1, n); velt_rt_write_byte(1, '\n');
        p->line++;
        if (p->line % ((p->id & 1) ? 1000 : 100) == 0) { velt_rt_yield_now(cx); return 0; }
    }
    return 1;
}
static void printer_drop(void* s) { (void)s; }
typedef struct { int32_t result; uint32_t tag; int64_t i; VeltFut* h[64]; } Main;
static uint32_t main_poll(void* s, void* cx) {
    Main* m = s;
    if (m->tag == 0) {
        int64_t i;
        for (i = 0; i < 128; i++) PAD[i] = 'x';
        for (i = 0; i < 64; i++) {
            Printer p = { 0, 0, i, 0 };
            m->h[i] = velt_rt_spawn(printer_poll, printer_drop, &p, sizeof p, 8, 0, 0);
        }
        m->tag = 1;
    }
    for (; m->i < 64; m->i++) {
        if (!velt_rt_fut_poll(m->h[m->i], cx)) return 0;
        velt_rt_fut_drop(m->h[m->i]);
    }
    return 1;
}
int32_t velt_main(void) { Main m = { 0 }; velt_rt_block_on(main_poll, &m); return 0; }
"#;

/// Check one stress line: `t=<id> l=<line> <n x's> <n>`; returns (id, line).
fn parse_stress_line(line: &str) -> (usize, usize) {
    let num = |s: Option<&str>| -> usize {
        s.and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("corrupted line: {line:?}"))
    };
    let parts: Vec<&str> = line.split(' ').collect();
    let [t, l, pad, n] = parts[..] else {
        panic!("corrupted line: {line:?}")
    };
    let (id, no, n) = (
        num(t.strip_prefix("t=")),
        num(l.strip_prefix("l=")),
        num(Some(n)),
    );
    let pad_ok = pad.len() == n && pad.bytes().all(|b| b == b'x');
    assert!(
        pad_ok && n == (id * 7 + no * 13) % 120,
        "corrupted line: {line:?}"
    );
    (id, no)
}

/// Lines from 64 tasks on 8 workers arrive whole and in order per task, and piped output is
/// written in large blocks: each run must stay under 250 ms of CPU time. It takes about 20 ms
/// (debug runtime, also with every core busy); one write per line takes about 500 ms. CPU time,
/// not wall-clock time, which on a loaded machine mostly measures waiting for a core (and on
/// macOS includes the first launch's code-signature check, over a second).
#[test]
fn concurrent_tasks_never_interleave_within_a_line() {
    use std::io::Read;
    let Some(exe) = build("stdout_stress", STDOUT_STRESS) else {
        eprintln!("NOTE: no C toolchain found; stdout stress check skipped");
        return;
    };
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let mut child = command(&exe)
            .env("VELT_THREADS", "8")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        let (status, cpu) = wait_with_cpu_time(child);
        let elapsed = start.elapsed();
        assert!(status.success(), "{status}");
        let text = text(&stdout);
        let mut next = vec![0usize; 64];
        let mut count = 0;
        for line in text.lines() {
            let (id, no) = parse_stress_line(line);
            assert_eq!(no, next[id], "task {id} lines out of order");
            next[id] += 1;
            count += 1;
        }
        assert_eq!(count, 64_000);
        eprintln!(
            "stdout stress: 64000 lines, {} bytes in {elapsed:?} ({cpu:?} CPU)",
            text.len()
        );
        assert!(
            cpu < Duration::from_millis(250),
            "stdout stress too slow: {cpu:?} CPU"
        );
    }
}

/// Waits for `child` and returns its exit status and the CPU time it used (user + system).
#[cfg(unix)]
fn wait_with_cpu_time(child: std::process::Child) -> (std::process::ExitStatus, Duration) {
    use std::os::unix::process::ExitStatusExt;
    let pid = child.id() as libc::pid_t;
    let mut status = 0;
    // SAFETY: a zeroed `rusage` is valid; `wait4` writes the status and usage of our child.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    while unsafe { libc::wait4(pid, &mut status, 0, &mut usage) } != pid {
        let e = std::io::Error::last_os_error();
        assert_eq!(e.kind(), std::io::ErrorKind::Interrupted, "wait4: {e}");
    }
    let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    (
        std::process::ExitStatus::from_raw(status),
        time(usage.ru_utime) + time(usage.ru_stime),
    )
}

/// Waits for `child` and returns its exit status and the CPU time it used (user + kernel).
#[cfg(windows)]
fn wait_with_cpu_time(mut child: std::process::Child) -> (std::process::ExitStatus, Duration) {
    use std::os::windows::io::AsRawHandle;
    /// `FILETIME`: 100 ns ticks.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let status = child.wait().unwrap();
    let mut t = [FileTime::default(); 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: the handle stays open until `child` drops; the four outputs are writable.
    let ok = unsafe { GetProcessTimes(child.as_raw_handle(), c, e, k, u) };
    assert_ne!(
        ok,
        0,
        "GetProcessTimes: {}",
        std::io::Error::last_os_error()
    );
    let ticks = |t: FileTime| (t.high as u64) << 32 | t.low as u64;
    (
        status,
        Duration::from_nanos((ticks(t[2]) + ticks(t[3])) * 100),
    )
}

/// `Command::new(program)` for a test's child process. On Windows, when this test process has no
/// console (a CI agent, a background shell), the child gets a hidden console instead of opening a
/// window of its own. In a terminal it shares the terminal's console as before, so Ctrl+C still
/// reaches it.
fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let cmd = std::process::Command::new(program);
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleWindow() -> *mut std::ffi::c_void;
        }
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // SAFETY: takes no arguments; returns this process's console window or null.
        if unsafe { GetConsoleWindow() }.is_null() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    };
    cmd
}
