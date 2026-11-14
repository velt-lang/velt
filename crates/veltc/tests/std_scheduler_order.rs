//! Tasks that one poll of the executor in Velt wakes in turn run in turn (#2968,
//! std/lang/hooks/sched_queue.velt `pushNext`): the first takes the waker's run-next slot, and a
//! later one queues behind it. The later wake used to take the slot and push the earlier task
//! behind it, so two spawned tasks whose promises settled in turn finished in reverse, and their
//! reactions ran in the opposite order to Node's. Each program prints what Node prints, at two,
//! four and eight workers.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built, then run `runs` times at each of 2, 4 and 8 workers: it prints `want` every time.
fn prints_in_order(name: &str, src: &str, want: &str, runs: usize) {
    // Workers only start where `#sched-thread` has threads (macOS and Linux for now).
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(name);
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    for threads in ["2", "4", "8"] {
        for run in 0..runs {
            let o = command(&exe).env("VELT_THREADS", threads).output().unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let at = format!("{name}, {threads} workers, run {run}");
            assert!(
                o.status.success(),
                "{at}: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            assert_eq!(out, want, "{at}");
        }
    }
}

/// `main` resolves two promises in turn, each awaited by a spawned task: the tasks finish, and
/// their join handles' reactions run, in that order.
#[test]
fn tasks_woken_in_turn_by_resolves_run_in_turn() {
    let src = r#"async function wait(p: Promise<number>): Promise<number> {
  return await p;
}
async function main() {
  let ra: (v: number) => void = (v: number) => {};
  let rb: (v: number) => void = (v: number) => {};
  const pa = new Promise<number>((r) => {
    ra = r;
  });
  const pb = new Promise<number>((r) => {
    rb = r;
  });
  const a = spawn(wait(pa));
  const b = spawn(wait(pb));
  a.then((x) => console.log("a", x));
  b.then((x) => console.log("b", x));
  await sleep(5);
  ra(1);
  rb(2);
}
"#;
    prints_in_order("woken_by_resolves", src, "a 1\nb 2\n", 5);
}

/// Two spawned tasks each await a timer set in turn: the timers fire in one turn, and the tasks'
/// reactions run in the order they were spawned.
#[test]
fn tasks_woken_in_turn_by_timers_run_in_turn() {
    let src = r#"async function make(): Promise<number> {
  await sleep(1);
  return 7;
}
async function take(p: Promise<number>): Promise<number> {
  return await p;
}
async function main() {
  const a = spawn(take(make()));
  const b = spawn(take(make()));
  a.then((x) => console.log("a", x));
  b.then((x) => console.log("b", x));
}
"#;
    prints_in_order("woken_by_timers", src, "a 7\nb 7\n", 5);
}
