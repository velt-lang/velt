// Single-threaded reverse-complement: the Rust #1 program (reverse-complement.rs) on a rayon
// pool of one thread.

#[path = "reverse-complement.rs"]
mod revcomp;

fn main() -> std::io::Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("rayon pool");
    revcomp::run()
}
