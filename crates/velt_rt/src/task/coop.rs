//! Cooperative scheduling for the runtime's own tasks: a task has a budget of polls per turn
//! (tokio's, which its own leaves spend too), and code that drives many children (a task's
//! started promises) stops and yields once it is spent, so one task cannot hold the host's
//! worker for long.

use std::future::Future;

/// Whether the current task may keep polling children this turn.
pub fn has_budget_remaining() -> bool {
    tokio::task::coop::has_budget_remaining()
}

/// Runs `f` without spending the current task's budget (a started promise's first poll belongs
/// to the code that started it).
pub fn unconstrained<F: Future>(f: F) -> impl Future<Output = F::Output> {
    tokio::task::unconstrained(f)
}

/// Yields to the scheduler: the task runs again after the other ready tasks had a turn.
pub async fn yield_now() {
    tokio::task::yield_now().await
}
