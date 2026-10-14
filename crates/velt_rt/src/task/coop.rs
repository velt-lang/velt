//! Cooperative scheduling for the runtime's own combinators, whatever the backend: a task has a
//! budget of polls per turn, and code that drives many children (`Promise.all`, a task's started
//! promises) stops and yields once it is spent, so one task cannot hold a worker for long (and
//! a join does not re-poll every child on every wake, bench/FINDINGS.md §7).
//!
//! The tokio backends keep the budget in tokio (its own leaves spend it too).

use std::future::Future;

/// Whether the current task may keep polling children this turn.
pub fn has_budget_remaining() -> bool {
    tokio::task::coop::has_budget_remaining()
}

/// Spends one unit of the current task's budget; yields first when it is spent.
pub async fn consume_budget() {
    tokio::task::coop::consume_budget().await
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
