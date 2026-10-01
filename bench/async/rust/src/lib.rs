//! Runtime selection shared by the async benchmarks: `<bench>` runs on tokio's multi-thread
//! runtime (one worker per core, like the Velt runtime), `<bench> current` on the current-thread
//! runtime (one thread, like Node's event loop).

use std::future::Future;

/// Build the runtime chosen by the first command-line argument and run `main` to completion.
pub fn run<F: Future<Output = ()>>(main: F) {
    let current = std::env::args().nth(1).as_deref() == Some("current");
    let mut builder = if current {
        tokio::runtime::Builder::new_current_thread()
    } else {
        tokio::runtime::Builder::new_multi_thread()
    };
    let rt = builder.enable_all().build().expect("tokio runtime");
    rt.block_on(main);
}
