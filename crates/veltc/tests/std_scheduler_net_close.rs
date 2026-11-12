//! Sockets' closes on the reactor of the executor in Velt (std/lang/hooks/sched_io_table.velt
//! `closing`, #2649): a program that opens and closes sockets under `VELT_SCHEDULER=std` frees
//! every block it counted, and on Windows a closed socket's poll completion never reaches the
//! source that took its place (std/lang/hooks/reactor_afd_poll.velt). Split from
//! std_scheduler_net.rs.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` (file `name`) built with `VELT_SCHEDULER=std` against std folder `std` (the
/// repository's when `None`): the binary.
fn build_std(dir: &Path, name: &str, src: &str, std: Option<&Path>) -> std::path::PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std");
    if let Some(std) = std {
        build.env("VELT_STD", std);
    }
    let o = build.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    exe
}

/// `exe` run with `threads` workers, counting its blocks (`VELT_RC_STATS`): its exit status
/// and output, stderr after stdout.
fn run_counted(exe: &Path, threads: &str) -> (bool, String) {
    let mut child = command(exe)
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: a lost wake would hang the test run.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!(
                "{} ({threads} workers): still running after a minute",
                exe.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
    .replace("\r\n", "\n");
    (o.status.success(), out)
}

/// Sockets opened and closed one after the other, each pair exchanging a byte: what every
/// socket's close leaves behind in the reactor shows up in the block counts or, in the
/// white-box copy below, as an event of a request that outlived its source.
const CHURN: &str = "import { connect, listen } from \"velt/net\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  let n = 0;
  for (let i = 0; i < 40; i++) {
    const c = await connect(`127.0.0.1:${l.port}`);
    const s = await l.accept();
    await s.write(\"x\");
    n += (await c.read()).length;
    c.close();
    n += (await s.read()).length;
    s.close();
  }
  l.close();
  console.log(n);
}
";

/// A socket closed just before the program ends leaves its reactor state behind (on Windows a
/// poll request waiting for a completion nobody takes): none of it may count as a leaked block
/// (`blocks=A/F` with A = F), with one worker and several.
#[test]
fn a_socket_program_frees_every_block_it_counted() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "churn", CHURN, None);
    for threads in ["1", "4"] {
        let (ok, out) = run_counted(&exe, threads);
        assert!(ok, "{threads} worker(s): {out}");
        assert!(out.starts_with("40\n"), "{threads} worker(s): {out}");
        let Some(stats) = out.lines().rev().find(|l| l.starts_with("rc stats:")) else {
            // Only the debug runtime counts (a `--release` test run links the release one).
            if cfg!(debug_assertions) {
                panic!("no rc stats in:\n{out}");
            }
            return;
        };
        let blocks = stats
            .split_whitespace()
            .find_map(|w| w.strip_prefix("blocks="))
            .and_then(|b| b.split_once('/'))
            .unwrap_or_else(|| panic!("no block counts in {stats}"));
        assert_eq!(blocks.0, blocks.1, "{threads} worker(s): {stats}");
    }
}

/// Windows' reactor (std/lang/hooks/reactor_afd_poll.velt): a poll completion queued on the
/// port when its socket closes comes later. Unless the socket left the reactor before its close
/// (`DEREGISTERS`, std/lang/hooks/sched_io_table.velt `closing`), it came with the source of a
/// socket that may be another by then, and submitted its poll again on a handle that may be
/// another socket's. White-box, in a copy of std whose requests remember their source's
/// generation and panic on a completion of a live request whose source moved on; a second copy
/// without the deregistration must panic, or the check checks nothing.
#[test]
fn a_closed_sockets_poll_completion_never_reaches_its_next_source() {
    if !cfg!(windows) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for deregisters in [true, false] {
        let std_copy = dir.path().join(format!("std_{deregisters}"));
        copy_tree(&manifest().join("../../std"), &std_copy);
        let read = |p: &str| {
            std::fs::read_to_string(std_copy.join(p))
                .unwrap()
                .replace("\r\n", "\n")
        };
        // The generation at a fixed offset: the copy's layout must be the one the patch knows.
        assert!(read("lang/hooks/sched_io.velt").contains("export const S_GEN: usize = 108;\n"));
        let poll = read("lang/hooks/reactor_afd_poll.velt");
        let patches = [
            (
                "const P_PENDING: usize = 84;\nconst P_SIZE: usize = 96;\n",
                "const P_PENDING: usize = 84;\nconst P_SIZE: usize = 96;\nconst P_GEN: usize = 88;\n",
            ),
            (
                "  m.st(p + P_SOURCE, source);\n",
                "  m.st(p + P_SOURCE, source);\n  m.st32(p + P_GEN, m.ld32(source + 108));\n",
            ),
            (
                "    m.lastingRelease(p);\n    return false;\n  }\n  const status",
                "    m.lastingRelease(p);\n    return false;\n  }\n  if (m.ld32(m.ld(p + P_SOURCE) + 108) !== m.ld32(p + P_GEN)) {\n    panic(\"stale event\");\n  }\n  const status",
            ),
        ];
        let mut patched = poll.clone();
        for (from, to) in patches {
            assert!(
                patched.contains(from),
                "reactor_afd_poll.velt has no {from:?}"
            );
            patched = patched.replace(from, to);
        }
        std::fs::write(std_copy.join("lang/hooks/reactor_afd_poll.velt"), patched).unwrap();
        if !deregisters {
            let afd = read("lang/hooks/reactor_afd.velt");
            let from = "export const DEREGISTERS: bool = true;";
            assert!(afd.contains(from));
            std::fs::write(
                std_copy.join("lang/hooks/reactor_afd.velt"),
                afd.replace(from, "export const DEREGISTERS: bool = false;"),
            )
            .unwrap();
        }
        let exe = build_std(
            dir.path(),
            &format!("churn_{deregisters}"),
            CHURN,
            Some(&std_copy),
        );
        for threads in ["1", "4"] {
            let (ok, out) = run_counted(&exe, threads);
            if deregisters {
                assert!(ok && out.starts_with("40\n"), "{threads} worker(s): {out}");
            } else {
                assert!(
                    !ok && out.contains("stale event"),
                    "no stale event without the deregistration, {threads} worker(s): {out}"
                );
            }
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
