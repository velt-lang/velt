//! Benchmark-style checks on a hot loop calling a small function 10M times:
//! - always: after `Speed`, the loop contains no call, and both versions compile to objects;
//! - `--ignored`: link both objects with a tiny Rust harness (needs `rustc`), run them and
//!   print the timings (`cargo test -p velt_opt --test bench -- --ignored --nocapture`).

mod common;
mod corpus;

use std::path::{Path, PathBuf};

use common::builder::count_calls;
use common::validate::assert_valid;
use velt_codegen_cl::{emit_object, host_triple, CodegenOptions};
use velt_opt::{optimize, OptLevel};
use velt_vir::vir::Program;

const ITERATIONS: i64 = 10_000_000;

fn programs() -> (Program, Program) {
    let base = corpus::control::hot_loop_program(ITERATIONS);
    let mut opt = base.clone();
    optimize(&mut opt, OptLevel::Speed);
    assert_valid(&opt);
    (base, opt)
}

#[test]
fn hot_loop_is_call_free_after_optimization() {
    let (base, opt) = programs();
    let main = |p: &Program| {
        p.funcs
            .iter()
            .find(|f| f.symbol == "velt_main")
            .cloned()
            .expect("velt_main")
    };
    assert_eq!(count_calls(&main(&base)), 1);
    assert_eq!(count_calls(&main(&opt)), 0, "{opt}");
    assert_eq!(opt.funcs.len(), 1, "`mix` is inlined and removed");
    for p in [&base, &opt] {
        let opts = CodegenOptions {
            target: host_triple(),
            optimize: true,
        };
        emit_object(p, &opts).expect("codegen");
    }
}

const HARNESS: &str = r#"
extern "C" { fn velt_main() -> i32; }
#[no_mangle] pub extern "C" fn write_i64(_v: i64) {}
#[no_mangle] pub extern "C" fn write_f64(_v: f64) {}
#[no_mangle] pub extern "C" fn panic(_m: *const u8) -> ! { std::process::exit(101) }
fn main() {
    let mut best = u128::MAX;
    let mut rc = 0;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        rc = unsafe { velt_main() };
        best = best.min(t.elapsed().as_micros());
    }
    println!("{rc} {best}");
}
"#;

fn rustc() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let ok = command(&rustc)
        .arg("--version")
        .output()
        .ok()?
        .status
        .success();
    ok.then_some(rustc)
}

/// Build and run; returns (exit value printed by the harness, best time in µs).
fn build_and_run(rustc: &str, dir: &Path, tag: &str, program: &Program) -> (i32, u128) {
    let obj = dir.join(format!("{tag}.{}", if cfg!(windows) { "obj" } else { "o" }));
    let exe = dir.join(format!("{tag}{}", std::env::consts::EXE_SUFFIX));
    let harness = dir.join("harness.rs");
    std::fs::write(&harness, HARNESS).expect("write harness");
    let opts = CodegenOptions {
        target: host_triple(),
        optimize: true,
    };
    std::fs::write(&obj, emit_object(program, &opts).expect("codegen")).expect("write object");
    let out = command(rustc)
        .args(["--edition", "2021", "-O", "--crate-name", "harness"])
        .arg(&harness)
        .arg("-o")
        .arg(&exe)
        .arg(format!("-Clink-arg={}", obj.display()))
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = command(&exe).output().expect("run benchmark");
    let text = String::from_utf8_lossy(&run.stdout).to_string();
    let mut parts = text
        .split_whitespace()
        .map(|s| s.parse::<i128>().expect("number"));
    let rc = parts.next().expect("rc") as i32;
    (rc, parts.next().expect("time") as u128)
}

#[test]
#[ignore = "benchmark: needs rustc; run with --ignored --nocapture"]
fn hot_loop_timing() {
    let Some(rustc) = rustc() else {
        eprintln!("note: rustc not available; skipping");
        return;
    };
    let dir: PathBuf = std::env::temp_dir().join(format!("velt_opt_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (base, opt) = programs();
    let (rc_base, t_base) = build_and_run(&rustc, &dir, "base", &base);
    let (rc_opt, t_opt) = build_and_run(&rustc, &dir, "opt", &opt);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(rc_base, rc_opt);
    eprintln!(
        "hot loop, {ITERATIONS} iterations: unoptimized VIR {t_base} µs, optimized VIR {t_opt} µs ({:.2}x)",
        t_base as f64 / t_opt.max(1) as f64
    );
}

#[path = "../../../tests/common/command.rs"]
mod command;
use command::command;
