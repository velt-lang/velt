//! Shared by the bench/db Rust baselines: the RESULT line format, the typed row, the size scale,
//! the tokio runtime choice and the unique names each run uses.
//!
//! Arguments: `quick` runs 1/20 of the sizes; `current` runs the async programs on tokio's
//! current-thread runtime (one thread, like Node's event loop) instead of the multi-thread
//! runtime (one worker per core, like the Velt runtime).

use std::future::Future;
use std::time::Instant;

fn has_arg(name: &str) -> bool {
    std::env::args().skip(1).any(|a| a == name)
}

/// 20 with the `quick` argument (1/20 of the sizes), else 1.
pub fn scale() -> i64 {
    if has_arg("quick") {
        20
    } else {
        1
    }
}

/// Runs `main` on the tokio runtime the arguments select (see the module docs).
pub fn block_on<F: Future>(main: F) -> F::Output {
    let mut builder = if has_arg("current") {
        tokio::runtime::Builder::new_current_thread()
    } else {
        tokio::runtime::Builder::new_multi_thread()
    };
    builder
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(main)
}

/// Prints `RESULT <workload> <ops> <checksum> <ms>` with the milliseconds since `t0`.
pub fn report(workload: &str, ops: i64, checksum: i64, t0: Instant) {
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!("RESULT {workload} {ops} {checksum} {ms}");
}

/// The typed row every implementation decodes into.
pub struct Row {
    pub id: i64,
    pub name: String,
    pub score: f64,
}

impl Row {
    pub fn checksum(&self) -> i64 {
        self.id + self.name.len() as i64 + (self.score * 2.0) as i64
    }
}

/// A name unique to this run (process id + time), for tables, keys and files.
pub fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{}_{nanos}", std::process::id())
}
