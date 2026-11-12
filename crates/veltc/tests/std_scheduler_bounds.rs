//! The executor in Velt with more than one worker, or against a copy of std with a module added:
//! each program is built with `VELT_SCHEDULER=std` only and checks one thing of that build (the
//! worker count's bound, a latch's buffer of wakers, a wake delivered while worker 0 decides the
//! program's end). Split from std_scheduler.rs by size.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers start only where `#sched-thread` has threads (macOS, Linux and Windows).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// `src` built with `VELT_SCHEDULER=std` (against the std in `std_dir`, or the repository's) and
/// run with each `(VELT_THREADS, VELT_RT_DEBUG_ALLOC)` of `runs`: exit code and output of each.
fn std_runs(name: &str, src: &str, std_dir: Option<&Path>, runs: &[(&str, &str)]) -> Vec<String> {
    std_runs_with(name, src, std_dir, runs, &[])
}

/// [`std_runs`] with more variables (`env`) for each run.
fn std_runs_with(
    name: &str,
    src: &str,
    std_dir: Option<&Path>,
    runs: &[(&str, &str)],
    env: &[(&str, &str)],
) -> Vec<String> {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir
        .path()
        .join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std");
    if let Some(std_dir) = std_dir {
        build.env("VELT_STD", std_dir);
    }
    let o = build.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    runs.iter()
        .map(|(threads, debug_alloc)| {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", debug_alloc)
                .env_remove("VELT_SCHEDULER")
                .env_remove("VELT_RESCUE")
                .envs(env.iter().copied())
                .output()
                .unwrap();
            format!(
                "{:?} {}{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .collect()
}

/// `VELT_THREADS` above 4096 is taken for a typo, like one that is not a number: the online
/// cores. The bound is checked after each digit (5000 got through when it was checked before
/// the last one).
#[test]
fn velt_threads_past_the_bound_runs_on_the_cores() {
    if !has_threads() {
        return;
    }
    let src = r#"import { workerCount } from "velt/task";

async function main() {
  console.log(`workers: ${workerCount()}`);
}
"#;
    let runs = std_runs(
        "worker_bound",
        src,
        None,
        &[
            ("x", "0"),
            ("5000", "0"),
            ("4097", "0"),
            ("40960", "0"),
            ("8", "0"),
        ],
    );
    assert_eq!(runs[4], "Some(0) workers: 8\n", "VELT_THREADS=8");
    assert!(!runs[0].contains("workers: 5000"), "{}", runs[0]);
    for (run, v) in runs[1..4].iter().zip(["5000", "4097", "40960"]) {
        assert_eq!(
            run, &runs[0],
            "VELT_THREADS={v} runs on the cores, as one not a number"
        );
    }
}

/// A latch opened with many waiters of other tasks while several workers run (sched_latch.velt
/// `openAs`): their wakers go to a buffer grown 4, 8, 16, 32, 64 and freed after each growth and
/// at the end, each free with the size the block was made with, which the debug runtime's
/// allocator checks (`VELT_RT_DEBUG_ALLOC=1` aborts on a wrong size). No std caller has two
/// waiters on one latch (a promise's settlement waits once), so the program reaches the latch
/// through a module added to a copy of std.
#[test]
fn a_latch_with_many_waiters_frees_its_waker_buffer_by_size() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let std_dir = dir.path().join("std");
    copy_dir(&manifest().join("../../std"), &std_dir);
    std::fs::create_dir_all(std_dir.join("latch_probe")).unwrap();
    std::fs::write(std_dir.join("latch_probe/index.velt"), LATCH_PROBE).unwrap();
    let runs = std_runs(
        "latch_many",
        LATCH_MANY,
        Some(&std_dir),
        &[("4", "1"), ("1", "1")],
    );
    for (run, threads) in runs.iter().zip(["4", "1"]) {
        assert_eq!(run, "Some(0) sum 2080\n", "VELT_THREADS={threads}");
    }
}

/// Worker 0's end-of-program scan (sched_end.velt `quiet`) against a wake delivered meanwhile by
/// another worker. The scan ends the program when every other worker is parked with nothing
/// queued, no wait is linked into a socket and no worker was woken during the scan; a worker that
/// unlinked a wait (or fired a sleep) before its task was queued, while still marked parked, made
/// it end the program with `main` waiting: exit 0, its output lost (#2723 on Windows, #2836 on a
/// Mac). The fix has two halves. A parked worker counts itself awake as soon as its reactor's
/// wait returns, before it delivers events or fires timers (sched_io.velt `park`): that covers a
/// delivery and a worker's own sleep. A wait leaves the count of linked waits only after its wake,
/// and the scan reads that count before the queues (sched_io_wait.velt `wakeAll`, `expire`,
/// sched_end.velt `busy`): that covers a delivery too, and the waits a worker expires on its way
/// to park, with its park word up.
///
/// Made deterministic with copies of std that pause: 200 ms before a delivery's wakes and 400 ms
/// after its count, 100 ms before a worker with timed waits expires them on its way to park and
/// 400 ms in an expiry between the unlink and the wake, 300 ms in the scan between the count and
/// the queues (and [`patch_sleep`]'s for a sleep). In each program a task pinned to a third
/// worker ends during such a pause, which makes worker 0 scan then (a worker going idle wakes
/// it): a reader and a writer ([`WAKE_SCAN`]), a task taking turns with a connection it follows
/// (`join`, [`FOLLOWS`], the program where Windows lost its output), a timed wait that expires
/// ([`DEADLINE`]) and a sleep ([`SLEEPER`]). The copies without parts of the fix show which
/// window each covers: each ends the program early where it must ("Some(0) ", nothing printed).
/// Runs of each program of [`a_wake_delivered_while_worker_0_scans_keeps_the_program_alive`]: a
/// broken copy must end early in one of them, a fixed copy must print in all.
const BROKEN_RUNS: usize = 8;

#[test]
fn a_wake_delivered_while_worker_0_scans_keeps_the_program_alive() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    // (variant, [(program, whether it ends early)]).
    let variants: [(&str, &[(&str, bool)]); 8] = [
        (
            "fixed",
            &[
                ("wake_scan", false),
                ("follows", false),
                ("deadline", false),
            ],
        ),
        // Neither half for a delivery: the count dropped before the wake, the worker parked.
        ("unguarded", &[("wake_scan", true), ("follows", true)]),
        // The worker counts itself awake: enough on its own.
        ("awake_only", &[("wake_scan", false)]),
        // The count after the wake and read before the queues: enough on its own.
        ("order_only", &[("wake_scan", false)]),
        // The order's half with the queues read before the count.
        ("queues_first", &[("wake_scan", true)]),
        // An expiry before the worker waits, counted out before its wake.
        ("expire_first", &[("deadline", true)]),
        // A worker woken by its own sleep (its pauses only, `patch_sleep`): awake at once...
        ("sleep_fixed", &[("sleeper", false)]),
        // ...or, without the first half, read parked after its sleep left the timers.
        ("sleep_unguarded", &[("sleeper", true)]),
    ];
    for (variant, programs) in variants {
        let std_dir = dir.path().join(format!("std_{variant}"));
        copy_dir(&manifest().join("../../std"), &std_dir);
        patch_std(&std_dir, variant);
        for &(program, ends_early) in programs {
            let (src, want) = match program {
                "wake_scan" => (WAKE_SCAN, "Some(0) read x\n"),
                "follows" => (FOLLOWS, "Some(0) a! b!\n"),
                "sleeper" => (SLEEPER, "Some(0) slept\n"),
                _ => (DEADLINE, "Some(0) readable false\n"),
            };
            // The pauses make the window likely, not certain, on a loaded machine (a worker can
            // still win the race once in a while): a copy without part of the fix must end early
            // in at least one of its runs, and the fixed copies must print in every run.
            let runs = std_runs_with(
                &format!("{program}_{variant}"),
                src,
                Some(&std_dir),
                &[("4", "0"); BROKEN_RUNS],
                &[("VELT_RESCUE", "0")],
            );
            if ends_early {
                assert!(
                    runs.iter().any(|run| run == "Some(0) "),
                    "{variant}, {program}: did not end early in any of {BROKEN_RUNS} runs: {runs:?}"
                );
            } else {
                for run in &runs {
                    assert_eq!(run, want, "{variant}, {program}");
                }
            }
        }
    }
}

/// Patches the std copy at `std_dir` for a variant of
/// [`a_wake_delivered_while_worker_0_scans_keeps_the_program_alive`]: the pauses, then the parts
/// of the fix the variant leaves out.
fn patch_std(std_dir: &Path, variant: &str) {
    if variant.starts_with("sleep_") {
        patch_sleep(std_dir, variant == "sleep_unguarded");
        return;
    }
    let mut wait = read_std(std_dir, "lang/hooks/sched_io_wait.velt");
    let mut io = read_std(std_dir, "lang/hooks/sched_io.velt");
    let mut end = read_std(std_dir, "lang/hooks/sched_end.velt");
    wait.push_str(PAUSE);
    io.push_str(&PAUSE.replace("now()", "wt.now()"));
    end.push_str(&PAUSE.replace("now()", "wt.now()"));
    patch(
        &mut wait,
        "  let n: isize = 0;\n",
        "  let n: isize = 0;\n  pause(200);\n",
    );
    patch(
        &mut wait,
        "  waiting(-n);\n}\n",
        "  waiting(-n);\n  pause(400);\n}\n",
    );
    let detached = "    const linked = detach(f);\n";
    patch(&mut wait, detached, &format!("{detached}    pause(400);\n"));
    let expire = "  let ms = wt.expire(b);\n";
    patch(
        &mut io,
        expire,
        &format!("  if (m.ld(b + IO_TIMERS) !== 0) {{\n    pause(100);\n  }}\n{expire}"),
    );
    let awake =
        "  if (parkWord !== 0) {\n    m.store32(parkWord, 0);\n    m.incOrdered(wakes);\n  }\n";
    let waits = "  if (ioWaits(g)) {\n    return true;\n  }\n";
    let queues = "  const n = m.load(g + t.E_STARTED);\n  for (let i: usize = 0; i < n; i++) {\n    if (workerBusy(t.worker(i), self)) {\n      return true;\n    }\n  }\n";
    let pause = "  pause(300);\n";
    let in_order = format!("{waits}{queues}");
    let count_first = (
        "  pause(200);\n",
        "  for (let x = w; x !== 0; x = m.ld(x + W_NEXT)) {\n    waiting(-1);\n  }\n  pause(200);\n",
    );
    // The scan's pause only where it hides nothing: with the count dropped before the wake, a
    // pause after reading it would find the task queued by then.
    match variant {
        "unguarded" => {
            patch(&mut io, awake, "");
            patch(&mut wait, count_first.0, count_first.1);
            patch(&mut wait, "  waiting(-n);\n", "");
        }
        "awake_only" => {
            patch(&mut wait, count_first.0, count_first.1);
            patch(&mut wait, "  waiting(-n);\n", "");
            patch(&mut end, &in_order, &format!("{queues}{pause}{waits}"));
        }
        "order_only" => {
            patch(&mut io, awake, "");
            patch(&mut end, &in_order, &format!("{waits}{pause}{queues}"));
        }
        "queues_first" => {
            patch(&mut io, awake, "");
            patch(&mut end, &in_order, &format!("{queues}{pause}{waits}"));
        }
        "expire_first" => {
            let counted = "    if (linked) {\n      waiting(-1);\n    }\n";
            patch(&mut wait, counted, "");
            patch(
                &mut wait,
                &format!("{detached}    pause(400);\n"),
                &format!("{detached}{counted}    pause(400);\n"),
            );
        }
        _ => patch(&mut end, &in_order, &format!("{waits}{pause}{queues}")),
    }
    std::fs::write(std_dir.join("lang/hooks/sched_io_wait.velt"), wait).unwrap();
    std::fs::write(std_dir.join("lang/hooks/sched_io.velt"), io).unwrap();
    std::fs::write(std_dir.join("lang/hooks/sched_end.velt"), end).unwrap();
}

/// Patches the std copy at `std_dir` for [`SLEEPER`]: pauses of 200 ms after a worker other
/// than worker 0 returns from its reactor's wait with no event (its timer's), 400 ms in a sleep's expiry between leaving the
/// timers and waking its task, 300 ms in the scan between worker 1's park word and its timers
/// once a timer woke a worker;
/// without the worker counting itself awake (`unguarded`).
fn patch_sleep(std_dir: &Path, unguarded: bool) {
    let mut io = read_std(std_dir, "lang/hooks/sched_io.velt");
    let mut sleep = read_std(std_dir, "lang/hooks/sched_sleep.velt");
    let mut end = read_std(std_dir, "lang/hooks/sched_end.velt");
    io.push_str(&PAUSE.replace("now()", "wt.now()"));
    sleep.push_str(&PAUSE.replace("now()", "clock()"));
    end.push_str(&PAUSE.replace("now()", "wt.now()"));
    let awake =
        "  if (parkWord !== 0) {\n    m.store32(parkWord, 0);\n    m.incOrdered(wakes);\n  }\n";
    // The scan's pause only while a timer's wake is in its pause (not in an earlier scan).
    io.push_str("export const TIMED_OUT = new Atomic<bool>(false);\n");
    let after = "  if (n === 0 && b !== (IO.load(\"relaxed\") as usize)) {\n    TIMED_OUT.store(true, \"seqcst\");\n    pause(200);\n  }\n";
    patch(
        &mut io,
        awake,
        &if unguarded {
            after.to_string()
        } else {
            format!("{awake}{after}")
        },
    );
    let unlinked = "    unlinkPending(b, f);\n";
    patch(
        &mut sleep,
        unlinked,
        &format!("{unlinked}    pause(400);\n"),
    );
    patch(
        &mut end,
        "  return waits(w);\n",
        "  if (io.TIMED_OUT.load(\"seqcst\") && w === t.worker(1)) {\n    pause(300);\n  }\n  return waits(w);\n",
    );
    std::fs::write(std_dir.join("lang/hooks/sched_io.velt"), io).unwrap();
    std::fs::write(std_dir.join("lang/hooks/sched_sleep.velt"), sleep).unwrap();
    std::fs::write(std_dir.join("lang/hooks/sched_end.velt"), end).unwrap();
}

/// A pause of `ms` milliseconds, spinning, for the std copies of
/// [`a_wake_delivered_while_worker_0_scans_keeps_the_program_alive`] (sched_io.velt and
/// sched_end.velt read the clock as `wt.now()`).
const PAUSE: &str = "
function pause(ms: u64): void {
  const until = now() + ms * 1000000;
  while (now() < until) {}
}
";

/// `main` on worker 0 waits for a reader pinned to worker 1, whose connection lives on that
/// worker's reactor; the writer, pinned to worker 2, answers it and ends 50 ms later.
const WAKE_SCAN: &str = r#"import { connect, listen, TcpListener } from "velt/net";

async function writer(l: TcpListener): Promise<void> {
  const s = await l.accept();
  await sleep(20);
  await s.write("x");
  s.close();
  await sleep(50);
}

async function reader(port: i64): Promise<string> {
  const c = await connect(`127.0.0.1:${port}`);
  const got = await c.readString();
  c.close();
  return got;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const w = spawn(writer(l), { worker: 2 });
  console.log(`read ${await spawn(reader(port), { worker: 1 })}`);
  await w;
  l.close();
}
"#;

/// std_scheduler_workers' `a_task_follows_a_socket_tasks_take_turns_with` with the connection's
/// owner pinned to worker 1: a task spawned from worker 2 uses the connection next, joins worker
/// 1 (`join`) and is woken there by the answer, which the echo server on worker 3 sends 20 ms
/// after the request; it ends 50 ms after that.
const FOLLOWS: &str = r#"import { connect, listen, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";

async function echo(ports: Channel<i64>): Promise<void> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  const s = await l.accept();
  for (let i = 0; i < 2; i++) {
    const got = await s.readString();
    if (i == 1) {
      await sleep(20);
    }
    await s.write(`${got}!`);
  }
  s.close();
  l.close();
  await sleep(50);
}

async function user(c: TcpStream): Promise<string> {
  await c.write("b");
  const got = await c.readString();
  c.close();
  return got;
}

async function starter(c: TcpStream): Promise<string> {
  return await spawn(user(c));
}

async function owner(port: i64): Promise<string> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("a");
  const a = await c.readString();
  const b = await spawn(starter(c), { worker: 2 });
  return `${a} ${b}`;
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(echo(ports), { worker: 3 });
  const port = (await ports.receive()) ?? 0;
  console.log(await spawn(owner(port), { worker: 1 }));
  await server;
}
"#;

/// A reader pinned to worker 1 waits 30 ms for its connection to be readable, which it never is:
/// worker 1 expires the wait on its way to park, and then only looks at its reactor (the task is
/// queued). The holder, pinned to worker 2, ends 250 ms after the reader started waiting and
/// hands the other end to `main`, which closes it after the reader is done.
const DEADLINE: &str = r#"import { connect, listen, TcpListener, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";

async function holder(l: TcpListener, ready: Channel<i64>): Promise<TcpStream> {
  const s = await l.accept();
  await ready.receive();
  await sleep(250);
  // Open until the reader is done: its close would make the connection readable.
  return s;
}

async function reader(port: i64, ready: Channel<i64>): Promise<string> {
  const c = await connect(`127.0.0.1:${port}`);
  // Nothing to read: the next wait for input waits (a connected socket is taken as readable).
  c.tryRead(new Uint8Array(8));
  await ready.send(1);
  const r = await c.readableWithin(30);
  c.close();
  return `readable ${r}`;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const ready = channel<i64>(1);
  const h = spawn(holder(l, ready), { worker: 2 });
  console.log(await spawn(reader(port, ready), { worker: 1 }));
  (await h).close();
  l.close();
}
"#;

/// A task pinned to worker 1 sleeps 50 ms: its own timer wakes worker 1, which then runs it.
/// A task pinned to worker 3 spins 100 ms and ends, which makes worker 0 scan while worker 1 is
/// between its wake and the sleeper's queueing.
const SLEEPER: &str = r#"async function sleeper(): Promise<string> {
  await sleep(50);
  return "slept";
}

async function trigger(): Promise<void> {
  const until = Date.now() + 100;
  while (Date.now() < until) {}
}

async function main() {
  const tr = spawn(trigger(), { worker: 3 });
  console.log(await spawn(sleeper(), { worker: 1 }));
  await tr;
}
"#;

/// A task that ends last on another worker gives the executor's reference to its block back
/// before it counts itself no longer live (sched_run.velt `finish`): once that count says no task
/// is live, worker 0 may end the program, and a block freed after that counts as leaked
/// (`VELT_RC_STATS`; #2825's `close_now` golden, 8 blocks allocated and 7 freed). A copy of std
/// pauses 300 ms after the count on another worker; a second copy gives the reference back after
/// that pause and must show the leak.
#[test]
fn a_task_ending_last_on_another_worker_frees_its_block_first() {
    if !has_threads() {
        return;
    }
    let dir = test_dir::TestDir::new();
    let counted = "    wk.release(k);\n    if (mk.liveDown(g, e) === 1 && e !== g) {\n      // The last live task: worker 0 may wait for that in `schedFinish`.\n      wk.unpark(g);\n    }\n";
    let pause = "    if (e !== g) {\n      pause(300);\n    }\n";
    for released_first in [true, false] {
        let std_dir = dir.path().join(format!("std_{released_first}"));
        copy_dir(&manifest().join("../../std"), &std_dir);
        let mut run = read_std(&std_dir, "lang/hooks/sched_run.velt");
        run.push_str(&PAUSE.replace("now()", "(performance.now() * 1000000.0) as u64"));
        let to = if released_first {
            format!("{counted}{pause}")
        } else {
            format!(
                "{}{pause}    wk.release(k);\n",
                counted.replacen("    wk.release(k);\n", "", 1)
            )
        };
        patch(&mut run, counted, &to);
        std::fs::write(std_dir.join("lang/hooks/sched_run.velt"), run).unwrap();
        let runs = std_runs_with(
            &format!("last_task_{released_first}"),
            LAST_TASK,
            Some(&std_dir),
            &[("4", "0")],
            &[("VELT_RC_STATS", "1"), ("VELT_RESCUE", "0")],
        );
        let stats = runs[0]
            .lines()
            .find_map(|l| l.strip_prefix("rc stats:"))
            .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
            .and_then(|b| b.split_once('/'))
            .map(|(a, f)| (a.to_string(), f.to_string()));
        assert!(runs[0].starts_with("Some(0) done\n"), "{}", runs[0]);
        let (a, f) = stats.unwrap_or_else(|| panic!("no block counts: {}", runs[0]));
        assert_eq!(
            a == f,
            released_first,
            "blocks {a}/{f}, the reference given back first: {released_first}"
        );
    }
}

/// `main` returns while a task pinned to worker 1 runs: worker 0 waits for it, the last live task.
const LAST_TASK: &str = r#"async function work(): Promise<void> {
  await yieldNow();
  console.log("done");
}

async function main() {
  spawn(work(), { worker: 1 });
}
"#;

/// File `rel` of the std copy at `std_dir`, with Unix line ends.
fn read_std(std_dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(std_dir.join(rel))
        .unwrap()
        .replace("\r\n", "\n")
}

/// Replaces `from` (which must be in `text` once) with `to`.
fn patch(text: &mut String, from: &str, to: &str) {
    assert_eq!(
        text.matches(from).count(),
        1,
        "not once in the std copy: {from:?}"
    );
    *text = text.replace(from, to);
}

/// Copies directory `from` to `to` (made), recursively.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// The module of [`a_latch_with_many_waiters_frees_its_waker_buffer_by_size`]'s std copy: the
/// runtime's latch entries (which only std may declare).
const LATCH_PROBE: &str = r#"declare function velt_rt_latch_new(): u64;
declare function velt_rt_latch_free(latch: u64);
declare function velt_rt_latch_open(latch: u64);
declare async function velt_rt_latch_wait(latch: u64): Promise<void>;

export function latchNew(): u64 {
  return velt_rt_latch_new();
}

export function latchOpen(l: u64): void {
  velt_rt_latch_open(l);
}

export function latchFree(l: u64): void {
  velt_rt_latch_free(l);
}

export async function latchWait(l: u64): Promise<void> {
  await velt_rt_latch_wait(l);
}
"#;

/// 64 tasks on worker 0 wait on one latch: each sends on `ready` and starts its wait in the same
/// poll, so all wait before `main` (woken by the last send) opens it.
const LATCH_MANY: &str = r#"import { channel, Channel } from "velt/sync";
import { latchFree, latchNew, latchOpen, latchWait } from "velt/latch_probe";

async function waiter(l: u64, ready: Channel<i64>, i: i64): Promise<i64> {
  ready.trySend(i);
  await latchWait(l);
  return i + 1;
}

async function main() {
  const l = latchNew();
  const ready = channel<i64>(64);
  const tasks: Promise<i64>[] = [];
  for (let i = 0; i < 64; i++) {
    tasks.push(spawn(waiter(l, ready, i), { worker: 0 }));
  }
  for (let i = 0; i < 64; i++) {
    await ready.receive();
  }
  latchOpen(l);
  let sum = 0;
  for (const t of tasks) sum += await t;
  latchFree(l);
  console.log(`sum ${sum}`);
}
"#;
