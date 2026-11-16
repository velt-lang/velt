//! An anchored task's sleeps move with it (#3097, std/lang/hooks/sched_sleep_carry.velt): sleepers
//! whose worker is kept saturated are offered to idle workers while their sleeps wait, some while
//! a sleep of theirs expires on the worker they leave. Every sleep fires exactly once (a lost one
//! hangs the program, killed after a minute; a doubled one counts twice), the blocks balance, and
//! across the runs some sleeper did move.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;
use sched_workers_support::{has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

const PROGRAM: &str = r#"import { currentWorker } from "velt/task";

class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
  moved: Atomic<i64> = new Atomic<i64>(0);
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

function sleeps(id: i64, r: i64): i64 {
  return 1 + ((id + r) % 3);
}

// Rounds of one to three sleeps of 0 to 3 ms, yielding while they wait: its worker may offer it
// while a sleep of it expires there.
async function sleeper(gate: shared<Gate>, id: i64): Promise<void> {
  let at = currentWorker();
  for (let r = 0; r < 12; r++) {
    const ps: Promise<void>[] = [];
    for (let j = 0; j < sleeps(id, r); j++) {
      ps.push(sleep((id * 7 + r * 3 + j) % 4).then(() => {
        gate.fired.add(1);
      }));
    }
    for (let y = 0; y < (id + r) % 5; y++) {
      await yieldNow();
      if (currentWorker() != at) {
        gate.moved.add(1);
        at = currentWorker();
      }
    }
    await Promise.all(ps);
    if (currentWorker() != at) {
      gate.moved.add(1);
      at = currentWorker();
    }
  }
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  const sleepers: Promise<void>[] = [];
  let want = 0;
  for (let id = 0; id < 64; id++) {
    sleepers.push(spawn(sleeper(gate, id)));
    for (let r = 0; r < 12; r++) {
      want += sleeps(id, r);
    }
  }
  await Promise.all(sleepers);
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load() == want ? "all" : `${gate.fired.load()} of ${want}`}`);
  console.log(`moved ${gate.moved.load() > 0}`);
}
"#;

/// Runs `exe` with `env`, killed after a minute: its exit code (`None` when killed) and output.
fn run(exe: &Path, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut child = command(exe)
        .env_remove("VELT_SCHEDULER")
        .env_remove("VELT_RESCUE")
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls the child for its exit under a one-minute cap, assumes no timing
        std::thread::sleep(Duration::from_millis(20));
    };
    let o = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (code, text)
}

/// Whether the `rc stats:` line shows as many blocks freed as allocated.
fn balanced(out: &str) -> bool {
    out.lines()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

#[test]
fn sleeps_move_with_their_task_and_fire_once() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("carry.velt");
    std::fs::write(&file, PROGRAM).unwrap();
    let exe = dir
        .path()
        .join(format!("carry{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // Four and two workers, many runs; one worker with the rescue, which may move tasks too.
    let modes: [(&[(&str, &str)], usize); 3] = [
        (&[("VELT_THREADS", "4")], 8),
        (&[("VELT_THREADS", "2")], 8),
        (&[("VELT_THREADS", "1"), ("VELT_RESCUE", "1")], 2),
    ];
    let mut moved = 0;
    for (env, runs) in modes {
        for _ in 0..runs {
            let (code, out) = run(&exe, env);
            assert_eq!(code, Some(0), "{env:?}: {out}");
            assert!(out.starts_with("fired all\n"), "{env:?}: {out}");
            assert!(balanced(&out), "{env:?}: blocks not balanced: {out}");
            if out.contains("moved true") {
                moved += 1;
            }
        }
    }
    assert!(moved > 0, "no sleeper moved in any run");
}
