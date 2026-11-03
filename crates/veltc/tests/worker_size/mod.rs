//! The size of a `velt build --host` Worker as Cloudflare's limit measures it (3 MB on the Free
//! plan, 10 MB on the Paid one): its `.wasm` and `.mjs` files gzipped one by one
//! (tests/workers.rs, tests/workerd.rs).

use std::path::Path;

/// The Worker's files in `dir`, gzipped one by one.
fn gzipped(dir: &Path) -> usize {
    use std::io::Write;
    let mut total = 0;
    for entry in std::fs::read_dir(dir).expect("worker dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += gzipped(&path);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("wasm" | "mjs")) {
            let bytes = std::fs::read(&path).expect("read");
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            gz.write_all(&bytes).expect("gzip");
            total += gz.finish().expect("gzip").len();
        }
    }
    total
}

/// Fails when the Worker in `worker` weighs more than `budget` bytes, its files gzipped one by one.
pub fn within_budget(worker: &Path, budget: usize) {
    let size = gzipped(worker);
    eprintln!("{}: {size} bytes gzipped", worker.display());
    assert!(
        size <= budget,
        "{}: {size} bytes gzipped, over the budget of {budget}",
        worker.display()
    );
}
