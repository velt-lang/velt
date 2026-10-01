// Single-threaded regex-redux: the Rust #6 program (regex-redux.rs) on a rayon pool of one
// thread.

#[path = "regex-redux.rs"]
mod regexredux;

fn main() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("rayon pool");
    regexredux::run()
}
