//! Which programs select the per-core scheduler (#211): `velt_main` calls
//! `velt_rt_set_scheduler` only in a program that pins a task (`spawn(p, { worker })`). One that
//! only calls `workerCount()` (to size a pool of unpinned tasks) or `currentWorker()` keeps the
//! work-stealing scheduler, which runs such pools faster.

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
fn selects_per_core(vir: &str) -> bool {
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
fn pinning_a_task_selects_per_core() {
    let vir = vir_of(
        "async function work(): Promise<void> {}

async function main() {
  await spawn(work(), { worker: 0 });
}
",
    );
    assert!(selects_per_core(&vir), "{vir}");
}

#[test]
fn worker_queries_alone_keep_the_stealing_scheduler() {
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
    assert!(!selects_per_core(&vir), "{vir}");
}

#[test]
fn programs_without_placement_keep_the_stealing_scheduler() {
    let vir = vir_of(
        "async function work(): Promise<i64> {
  return 1;
}

async function main() {
  console.log(await spawn(work()));
}
",
    );
    assert!(!selects_per_core(&vir), "{vir}");
}
