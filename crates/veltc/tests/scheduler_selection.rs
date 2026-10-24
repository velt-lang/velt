//! Which programs call `velt_rt_set_scheduler` (#211): `velt_main` does so only in a program
//! that pins a task (`spawn(p, { worker })`), not in one that only calls `workerCount()` or
//! `currentWorker()`. The default scheduler keeps pinned tasks on their workers itself, so the
//! call changes nothing at run time; it stays in the ABI for programs compiled to make it.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
fn vir_of(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether `velt_main` calls `velt_rt_set_scheduler`.
fn calls_set_scheduler(vir: &str) -> bool {
    let mut in_main = false;
    for line in vir.lines() {
        if line.starts_with("fn#") {
            in_main = line.contains(" velt_main(");
        } else if in_main && line.contains("velt_rt_set_scheduler(") {
            return true;
        }
    }
    false
}

#[test]
fn pinning_a_task_calls_set_scheduler() {
    let vir = vir_of(
        "async function work(): Promise<void> {}

async function main() {
  await spawn(work(), { worker: 0 });
}
",
    );
    assert!(calls_set_scheduler(&vir), "{vir}");
}

#[test]
fn worker_queries_alone_make_no_scheduler_call() {
    let vir = vir_of(
        "import { workerCount, currentWorker } from \"velt:task\";

async function work(i: i64): Promise<i64> {
  return i + currentWorker();
}

async function main() {
  const tasks: Promise<i64>[] = [];
  for (let i = 0; i < workerCount(); i++) {
    tasks.push(spawn(work(i)));
  }
  console.log((await Promise.all(tasks)).length);
}
",
    );
    assert!(!calls_set_scheduler(&vir), "{vir}");
}

#[test]
fn programs_without_placement_make_no_scheduler_call() {
    let vir = vir_of(
        "async function work(): Promise<i64> {
  return 1;
}

async function main() {
  console.log(await spawn(work()));
}
",
    );
    assert!(!calls_set_scheduler(&vir), "{vir}");
}
