//! `Promise.all` over an array under the executor in Velt (std/lang/hooks/sched_all.velt, #2944):
//! children that the runtime wakes from its own threads (file reads on its blocking pool), and
//! children that are other tasks, on other workers, so only the join's wake of its awaiter brings
//! `main` back; with one worker, with a lazy worker 0 (`VELT_RESCUE=1`) and with four.

use std::path::{Path, PathBuf};

use sched_workers_support::{build_and_run_with, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// Builds `src` as `name` in `dir` (one run with `threads` and `env` checked against `want`), then
/// runs it `runs` more times: each output path is linked once (relinking an executable that just
/// ran can fail on Windows).
fn build_then_run(
    dir: &Path,
    name: &str,
    src: &str,
    (threads, env): (&str, &[(&str, &str)]),
    want: &str,
    runs: usize,
) {
    let (code, out) = build_and_run_with(dir, name, src, threads, env);
    assert_eq!((code, out.as_str()), (0, want), "{name}: first run");
    let exe: PathBuf = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    for run in 0..runs {
        let o = no_window::command(&exe)
            .env("VELT_THREADS", threads)
            .env_remove("VELT_SCHEDULER")
            .env_remove("VELT_RESCUE")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
        assert_eq!(
            (o.status.code(), out.as_str()),
            (Some(0), want),
            "{name}: run {run}, stderr: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// A mode to run in: its tag (in the program's name), `VELT_THREADS`, more variables.
type Mode = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// The modes: one worker, a lazy worker 0 with the rescue, four workers with and without it.
const MODES: [Mode; 4] = [
    ("t1", "1", &[]),
    ("t1_rescue", "1", &[("VELT_RESCUE", "1")]),
    ("t4", "4", &[]),
    ("t4_no_rescue", "4", &[("VELT_RESCUE", "0")]),
];

/// Rounds of `Promise.all` over 64 file reads: each read's wake comes from a thread of the
/// runtime's, while the join may be polled, its awaiter replaced or the join dropped on the
/// worker (sched_all.velt's `J_WAKING`). Every round's results are checked, in array order.
#[test]
fn a_join_over_reads_the_runtime_wakes_from_its_threads_completes_in_every_mode() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    for i in 0..64 {
        std::fs::write(data.join(format!("f{i}.txt")), "x".repeat(i + 1)).unwrap();
    }
    let data = data.to_str().unwrap().to_string();
    for (tag, threads, extra) in MODES {
        let mut env = vec![("DATA", data.as_str())];
        env.extend_from_slice(extra);
        let name = format!("reads_{tag}");
        let want = "rounds 30, total 62400\n";
        build_then_run(dir.path(), &name, READS, (threads, &env), want, 4);
    }
}

/// The program of [`a_join_over_reads_the_runtime_wakes_from_its_threads_completes_in_every_mode`].
const READS: &str = r#"import { readFile } from "velt/fs";

async function main() {
  const dir = process.env.DATA ?? ".";
  let total = 0;
  for (let r = 0; r < 30; r++) {
    const ps: Promise<string>[] = [];
    for (let i = 0; i < 64; i++) {
      ps.push(readFile(`${dir}/f${i}.txt`));
    }
    const texts = await Promise.all(ps);
    for (let i = 0; i < 64; i++) {
      if (texts[i].length != i + 1) {
        console.log(`round ${r}: file ${i} has ${texts[i].length} bytes`);
      }
      total += texts[i].length;
    }
  }
  console.log(`rounds 30, total ${total}`);
}
"#;

/// Rounds of `Promise.all` over 500 spawned tasks that sleep and yield: the children are other
/// tasks, finished on any worker, so `main`'s task is woken only by the join's wake of its awaiter
/// (sched_all.velt `allWakeByRef`), never by a child of its own. Without that wake `main` never
/// resumes, and the program ends early with nothing printed.
#[test]
fn a_join_over_spawned_tasks_wakes_its_awaiter_in_every_mode() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for (tag, threads, extra) in MODES {
        let name = format!("spawned_{tag}");
        let want = "rounds 10, total 1247500\n";
        build_then_run(dir.path(), &name, SPAWNED, (threads, extra), want, 4);
    }
}

/// The program of [`a_join_over_spawned_tasks_wakes_its_awaiter_in_every_mode`].
const SPAWNED: &str = r#"async function child(i: i64): Promise<i64> {
  await sleep(1 + (i % 3));
  await yieldNow();
  return i;
}

async function main() {
  let total: i64 = 0;
  for (let r = 0; r < 10; r++) {
    const hs: Promise<i64>[] = [];
    for (let i: i64 = 0; i < 500; i++) {
      hs.push(spawn(child(i)));
    }
    for (const v of await Promise.all(hs)) {
      total += v;
    }
  }
  console.log(`rounds 10, total ${total}`);
}
"#;
