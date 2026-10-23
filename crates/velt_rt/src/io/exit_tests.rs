//! Output written before the process ends abnormally reaches the OS first, in order: an exit with
//! a code (an uncaught error, `process.exit`) and a panic or fatal runtime error, also while
//! another thread holds the shared buffer's lock. Each case runs in a child process (this test
//! binary again, told by an environment variable which case to play), whose stdout is a pipe.

use std::process::{Output, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use super::stdout;

/// Names the case the child process plays.
const CHILD: &str = "VELT_RT_EXIT_FLUSH_CHILD";

/// Lines of output the cases print before they end: several times the buffer size, so some of
/// it has gone through the shared buffer and some is still in the thread's own.
fn lines() -> String {
    (0..6000)
        .map(|i| format!("line {i:05} before the end\n"))
        .collect()
}

/// Run test `name` in a child process playing `case`.
fn run_child(name: &str, case: &str) -> Output {
    use std::io::Read;
    let exe = std::env::current_exe().expect("test executable");
    let mut child = crate::abi_tests::command::command(exe)
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, case)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the child");
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = r.read_to_end(&mut v);
            v
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let err = drain(Box::new(child.stderr.take().expect("piped stderr")));
    // A hang guard, not a time limit: the child exits at once unless its exit deadlocks.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the process hung at exit ({case})");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    }
}

/// The child's stdout without the test harness's own lines (`running 1 test` comes first, and
/// nothing after the case's exit).
fn program_output(o: &Output) -> String {
    let text = String::from_utf8_lossy(&o.stdout);
    let start = text.find("line 00000").unwrap_or(text.len());
    text[start..].to_string()
}

/// Which case this process plays (none in the parent).
fn case() -> Option<String> {
    std::env::var(CHILD).ok()
}

/// Print [`lines`] the way compiled code does, from this thread and from one that ends first.
fn print_lines() {
    let text = lines();
    let (first, rest) = text.split_at(text.len() / 3);
    let first = first.to_string();
    // Another thread's output, published when it ends.
    std::thread::spawn(move || stdout::append(|b| b.extend_from_slice(first.as_bytes())))
        .join()
        .expect("printing thread");
    for line in rest.split_inclusive('\n') {
        stdout::append(|b| b.extend_from_slice(line.as_bytes()));
    }
}

/// Hold the shared buffer's lock on another thread (as one writing it out does) until a flush
/// on the way out waits for it.
fn hold_shared_lock_elsewhere() {
    let (locked, is_locked) = mpsc::channel();
    std::thread::spawn(move || {
        let _held = stdout::lock_for_test();
        locked.send(()).expect("signal");
        while !stdout::EXIT_WAITED.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
    });
    is_locked.recv().expect("the lock is taken");
}

#[test]
fn an_exit_code_flushes_everything_printed_before() {
    if case().as_deref() == Some("exit") {
        print_lines();
        crate::panic::velt_rt_exit(1);
    }
    let o = run_child(
        "io::exit_tests::an_exit_code_flushes_everything_printed_before",
        "exit",
    );
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(
        program_output(&o) == lines(),
        "stdout lost or reordered output"
    );
}

#[test]
fn a_fatal_error_flushes_while_another_thread_holds_the_lock() {
    if case().as_deref() == Some("fatal-locked") {
        print_lines();
        hold_shared_lock_elsewhere();
        crate::panic::fatal("boom");
    }
    let o = run_child(
        "io::exit_tests::a_fatal_error_flushes_while_another_thread_holds_the_lock",
        "fatal-locked",
    );
    assert_eq!(
        o.status.code(),
        Some(crate::panic::PANIC_EXIT_CODE),
        "{o:?}"
    );
    assert!(
        program_output(&o) == lines(),
        "stdout lost or reordered output"
    );
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("panic: boom"),
        "{o:?}"
    );
}

#[test]
fn a_fatal_error_under_the_lock_does_not_hang() {
    if case().as_deref() == Some("fatal-own-lock") {
        stdout::append(|b| b.extend_from_slice(b"line 00000 mine\n"));
        let _held = stdout::lock_for_test();
        crate::panic::fatal("boom");
    }
    let o = run_child(
        "io::exit_tests::a_fatal_error_under_the_lock_does_not_hang",
        "fatal-own-lock",
    );
    assert_eq!(
        o.status.code(),
        Some(crate::panic::PANIC_EXIT_CODE),
        "{o:?}"
    );
    assert_eq!(program_output(&o), "line 00000 mine\n");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("panic: boom"),
        "{o:?}"
    );
}
