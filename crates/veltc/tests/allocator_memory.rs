//! The allocator commits memory as it is used (velt_rt `mem::configure`). Committed eagerly, as
//! mimalloc does by default where memory is overcommitted (Linux), an arena is marked for
//! transparent huge pages and each worker's first allocations made 2 MiB resident: a server
//! allocating on every worker used several times the memory its data needed.

#![cfg(target_os = "linux")]

mod no_window;
mod test_dir;

/// A program allocating a little on each worker, then printing its resident set size.
const PROGRAM: &str = "import { workerCount } from \"velt:task\";

async function fill(i: i64): Promise<i64> {
  const kept: i64[][] = [];
  for (let k = 0; k < 2000; k++) {
    kept.push([i, k]);
  }
  return kept.length as i64;
}

async function main() {
  const tasks: Promise<i64>[] = [];
  for (let i = 0; i < workerCount(); i++) {
    tasks.push(spawn(fill(i), { worker: i }));
  }
  let n: i64 = 0;
  for (const t of tasks) {
    n += await t;
  }
  console.log(n, process.memoryUsage().rss);
}
";

/// The resident set size the program reports on `threads` workers, with `env` set.
fn rss(dir: &std::path::Path, threads: u32, env: &[(&str, &str)]) -> u64 {
    let mut cmd = crate::no_window::command(dir.join("main"));
    cmd.env("VELT_THREADS", threads.to_string());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut words = text.split_whitespace();
    let want = (threads * 2000).to_string();
    assert_eq!(words.next(), Some(want.as_str()), "{text}");
    words.next().and_then(|w| w.parse().ok()).expect("rss")
}

#[test]
fn workers_allocating_a_little_keep_the_process_small() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), PROGRAM).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "-o", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lazy = rss(dir.path(), 8, &[]);
    // Huge pages only where the kernel hands them out on request.
    let thp = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled")
        .is_ok_and(|s| s.contains("[madvise]") || s.contains("[always]"));
    if thp {
        // What 7 more workers add, so the process's fixed part (the runtime, the binary, the
        // main thread) doesn't count: committed eagerly each worker's first allocations make
        // 2 MiB resident, as they use it a few KiB. Comparing whole processes failed when the
        // fixed part grew to half of the eager total.
        let grown =
            |env: &[(&str, &str)]| rss(dir.path(), 8, env).saturating_sub(rss(dir.path(), 1, env));
        let (lazy_grown, eager_grown) =
            (grown(&[]), grown(&[("MIMALLOC_ARENA_EAGER_COMMIT", "1")]));
        assert!(
            lazy_grown * 2 < eager_grown,
            "7 more workers committing as used: {lazy_grown} more bytes resident; eagerly: {eager_grown}"
        );
    }
    assert!(lazy < 24 << 20, "{lazy} bytes resident");
}
