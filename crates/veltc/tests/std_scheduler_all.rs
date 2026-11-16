//! `Promise.all` over an array under the executor in Velt (std/lang/hooks/sched_all.velt, #2944):
//! children that the runtime wakes from its own threads (file reads on its blocking pool), with
//! one worker, with a lazy worker 0 (`VELT_RESCUE=1`) and with four.

use sched_workers_support::{build_and_run_with, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

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
    let modes: [(&str, &[(&str, &str)]); 3] =
        [("1", &[]), ("1", &[("VELT_RESCUE", "1")]), ("4", &[])];
    for (threads, extra) in modes {
        let mut env = vec![("DATA", data.as_str())];
        env.extend_from_slice(extra);
        for run in 0..5 {
            let (code, out) = build_and_run_with(dir.path(), "reads", READS, threads, &env);
            assert_eq!(
                (code, out.as_str()),
                (0, "rounds 30, total 62400\n"),
                "threads {threads} {extra:?}, run {run}"
            );
        }
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
