//! Gather-back (#3097, std/lang/hooks/sched_gather.velt): tasks a burst spread over the workers
//! come back to worker 0 once the load is gone. Actors that worker 0's saturated queue offered to
//! idle workers during a burst then answer low-rate calls from `main`, each call waking their
//! worker from its reactor: by the last round every actor answers on worker 0. Across the runs
//! some actor did spread (else the test shows nothing), and the blocks balance.

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
import { channel, Channel } from "velt/sync";

class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
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

// Answers each call with the worker it runs on, after a little work (a burst's call queues it
// among the busy tasks for a while).
async function actor(calls: Channel<i64>, answers: Channel<i64>): Promise<void> {
  while (true) {
    const v = await calls.receive();
    if (v === null) {
      break;
    }
    // In parts, yielding between them: the actor waits in its worker's queue, which an offer
    // takes from, as a task with work to do.
    let x = 0;
    for (let part = 0; part < 4; part++) {
      for (let i = 0; i < 2000; i++) {
        x += (i + v) % 5;
      }
      await yieldNow();
    }
    await answers.send(x < 0 ? -1 : currentWorker());
  }
}

const ACTORS = 16;

async function main() {
  const calls: Channel<i64>[] = [];
  const answers: Channel<i64>[] = [];
  const actors: Promise<void>[] = [];
  for (let a = 0; a < ACTORS; a++) {
    calls.push(channel<i64>(1));
    answers.push(channel<i64>(1));
    actors.push(spawn(actor(calls[a], answers[a])));
  }
  // The burst: worker 0's queue waits long, so it offers tasks, actors among them.
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  let spread = false;
  // Every actor called at once, then the answers: all of them queue with the busy tasks.
  for (let r = 0; r < 40; r++) {
    for (let a = 0; a < ACTORS; a++) {
      await calls[a].send(r);
    }
    for (let a = 0; a < ACTORS; a++) {
      if ((await answers[a].receive()) != 0) {
        spread = true;
      }
    }
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  // Low rate: a call per actor, then a pause in which their workers go back to sleep.
  let away = 0;
  for (let r = 0; r < 80; r++) {
    away = 0;
    for (let a = 0; a < ACTORS; a++) {
      await calls[a].send(r);
      if ((await answers[a].receive()) != 0) {
        away++;
      }
    }
    await sleep(1);
  }
  for (let a = 0; a < ACTORS; a++) {
    calls[a].close();
  }
  await Promise.all(actors);
  console.log(`spread ${spread}`);
  console.log(`away at the end ${away}`);
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
fn actors_a_burst_spread_come_back_to_worker_0() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("gather.velt");
    std::fs::write(&file, PROGRAM).unwrap();
    let exe = dir
        .path()
        .join(format!("gather{}", if cfg!(windows) { ".exe" } else { "" }));
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
    let mut spread = 0;
    for _ in 0..6 {
        let (code, out) = run(&exe, &[("VELT_THREADS", "4")]);
        assert_eq!(code, Some(0), "{out}");
        assert!(balanced(&out), "blocks not balanced: {out}");
        if out.contains("spread true") {
            spread += 1;
            assert!(
                out.contains("away at the end 0\n"),
                "spread actors stayed away: {out}"
            );
        }
    }
    assert!(spread > 0, "no actor spread in any run");
}
