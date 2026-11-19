//! `performance.now()` counts from the start of the process: `velt_main` stamps its time origin
//! first thing (std/lang/hooks/clock.velt `clockStart`), so the first read in `main` is under a
//! millisecond and never negative. (Node's is tens of milliseconds, its own start-up included, so
//! a golden can't check this.) A program that only reads `Date.now()` stamps it too.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// Builds `src` and returns the first line it prints, as a number.
fn first_read(name: &str, src: &str) -> f64 {
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    let built = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = command(&exe).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8_lossy(&run.stdout);
    out.lines()
        .next()
        .unwrap_or("")
        .trim()
        .parse()
        .unwrap_or(f64::NAN)
}

#[test]
fn performance_now_starts_near_zero() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    // timing: what this checks is the time origin itself.
    let t = first_read(
        "origin",
        "function main() {\n  console.log(performance.now());\n}\n",
    );
    assert!(
        (0.0..1.0).contains(&t),
        "first performance.now() in main: {t} ms"
    );
    // A program reading the other clock first stamps the same origin.
    let t = first_read(
        "origin_date",
        "function main() {\n  const d = Date.now();\n  console.log(performance.now());\n  \
         console.log(d > 0);\n}\n",
    );
    assert!((0.0..1.0).contains(&t), "after Date.now(): {t} ms");
    // The origin is the start, not the first read: 20 ms spent before it count.
    let t = first_read(
        "origin_late",
        "function main() {\n  const start = Date.now();\n  let spins = 0;\n  \
         while (Date.now() - start < 20) {\n    spins++;\n  }\n  console.log(performance.now());\n  \
         console.log(spins > 0);\n}\n",
    );
    assert!(t >= 19.0, "after 20 ms in main: {t} ms");
}
