//! The compiler's pipeline thread: every stage runs on one thread with a large stack.

use super::BuildError;

/// Stack for the pipeline thread (reserved, not committed, until used). At least the parser's,
/// so the modules are parsed on it instead of each on a thread of its own.
const PIPELINE_STACK_BYTES: usize = 256 << 20;

/// Run a pipeline stage on the dedicated large-stack thread.
pub(super) fn on_pipeline_thread<T: Send>(
    stage: impl FnOnce() -> Result<T, BuildError> + Send,
) -> Result<T, BuildError> {
    std::thread::scope(|s| {
        let parse_here = || velt_syntax::with_stack_of(PIPELINE_STACK_BYTES, stage);
        let spawned = std::thread::Builder::new()
            .name("velt-compile".into())
            .stack_size(PIPELINE_STACK_BYTES)
            .spawn_scoped(s, parse_here);
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(e) => Err(BuildError::Failed(format!(
                "cannot start the compiler thread: {e}"
            ))),
        }
    })
}
