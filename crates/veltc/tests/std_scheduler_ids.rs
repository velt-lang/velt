//! The executor in Velt's task ids and its refusal inside a `read` callback (#2649 P2c part 1,
//! P2d): the golden `lang/rwlock_read_tasks_stop` and a cross-task `new Promise`, which no golden
//! run checks under `VELT_SCHEDULER=std` yet. Each program is built with `VELT_SCHEDULER=std` and
//! run with one worker and with the default count.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Whether a `VELT_SCHEDULER=std` program builds here (std_scheduler.rs `std_builds_here`).
fn std_builds_here() -> bool {
    cfg!(unix)
}

/// What a run gave: its exit code, stdout and stderr.
struct Run {
    code: Option<i32>,
    out: String,
    err: String,
}

/// `src` built with `VELT_SCHEDULER=std`, run with one worker and with the default count: each
/// run, labelled.
fn runs(name: &str, src: &str) -> Vec<(String, Run)> {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir
        .path()
        .join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let mut out = vec![];
    for threads in [Some("1"), None] {
        let mut run = command(&exe);
        run.env_remove("VELT_SCHEDULER");
        match threads {
            Some(t) => run.env("VELT_THREADS", t),
            None => run.env_remove("VELT_THREADS"),
        };
        let o = run.output().unwrap();
        let label = format!("{name} (threads: {threads:?})");
        out.push((
            label,
            Run {
                code: o.status.code(),
                out: String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
                err: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
        ));
    }
    out
}

/// P2d: a task started inside a `read` callback stops the program with the runtime's message,
/// before `main` goes on.
#[test]
fn a_task_started_in_a_read_callback_stops_the_program() {
    if !std_builds_here() {
        return;
    }
    let src = "class Item {
  v: number = 1;
}
class St {
  items: Item[] = [new Item()];
}
async function hold(i: Item): Promise<number> {
  await Promise.resolve(0);
  return i.v;
}
async function main() {
  const rw = shared(new RwLock<St>(new St()));
  const p = await hold(new Item());
  console.log(\"outside\", p);
  rw.read((s) => {
    spawn(hold(s.items[0]));
  });
  console.log(\"not reached\");
}
";
    for (label, r) in runs("read_spawn", src) {
        assert_eq!(r.code, Some(101), "{label}: {}{}", r.out, r.err);
        assert_eq!(r.out, "outside 1\n", "{label}: {}", r.err);
        assert!(
            r.err.contains(
                "a promise or task cannot start inside a `read` callback: what it holds would \
                 outlive the callback"
            ),
            "{label}: {}",
            r.err
        );
    }
}

/// P2c part 1: a promise made on one task and resolved on another gets a copy of the value
/// (std/lang/promise/settle.velt compares the tasks' ids), so a later change by the resolving
/// task isn't seen by the awaiting one.
#[test]
fn a_promise_resolved_on_another_task_gets_a_copy() {
    if !std_builds_here() {
        return;
    }
    let src = "class Box {
  v: number = 1;
}
async function other(resolve: (b: Box) => void): Promise<void> {
  const b = new Box();
  resolve(b);
  await yieldNow();
  b.v = 2;
  console.log(\"other sees\", b.v);
}
async function main() {
  const { promise, resolve } = Promise.withResolvers<Box>();
  const t = spawn(other(resolve));
  const got = await promise;
  await t;
  console.log(\"main sees\", got.v);
}
";
    for (label, r) in runs("promise_copy", src) {
        assert_eq!(r.code, Some(0), "{label}: {}{}", r.out, r.err);
        assert_eq!(r.out, "other sees 2\nmain sees 1\n", "{label}: {}", r.err);
    }
}
