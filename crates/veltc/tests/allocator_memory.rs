//! The allocator commits memory as it is used (velt_rt `mem::configure`). Committed eagerly, as
//! mimalloc does by default where memory is overcommitted (Linux), an arena is marked for
//! transparent huge pages and each worker's first allocations made 2 MiB resident: a server
//! allocating on every worker used several times the memory its data needed.

#![cfg(target_os = "linux")]

mod no_window;
mod test_dir;

/// A program allocating a little on each of 8 workers, then printing its resident set size.
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

/// The resident set size the program reports, with `env` set.
fn rss(dir: &std::path::Path, env: &[(&str, &str)]) -> u64 {
    let mut cmd = crate::no_window::command(dir.join("main"));
    cmd.env("VELT_THREADS", "8");
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
    assert_eq!(words.next(), Some("16000"), "{text}");
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
    let lazy = rss(dir.path(), &[]);
    // Huge pages only where the kernel hands them out on request.
    let thp = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled")
        .is_ok_and(|s| s.contains("[madvise]") || s.contains("[always]"));
    if thp {
        let eager = rss(dir.path(), &[("MIMALLOC_ARENA_EAGER_COMMIT", "1")]);
        assert!(
            lazy * 2 < eager,
            "committing as used: {lazy} bytes resident; eagerly: {eager}"
        );
    }
    assert!(lazy < 24 << 20, "{lazy} bytes resident");
}
