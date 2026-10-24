//! Cooperative scheduling for the runtime's own combinators, whatever the backend: a task has a
//! budget of polls per turn, and code that drives many children (`Promise.all`, a task's started
//! promises) stops and yields once it is spent, so one task cannot hold a worker for long (and
//! a join does not re-poll every child on every wake, bench/FINDINGS.md §7).
//!
//! The tokio backends keep the budget in tokio (its own leaves spend it too); the Velt backend in
//! its workers ([`super::runtime`]'s `velt::coop`).

use super::runtime::{self, velt_coop};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Whether the Velt backend drives this thread (its budget, not tokio's).
fn velt_here() -> bool {
    runtime::is_velt() && velt_coop::driven()
}

/// Whether the current task may keep polling children this turn.
pub fn has_budget_remaining() -> bool {
    if velt_here() {
        velt_coop::has_budget_remaining()
    } else {
        tokio::task::coop::has_budget_remaining()
    }
}

/// Spends one unit of the current task's budget; yields first when it is spent.
pub async fn consume_budget() {
    if !velt_here() {
        return tokio::task::coop::consume_budget().await;
    }
    if !velt_coop::spend() {
        YieldNow(false).await;
    }
}

/// Runs `f` without spending the current task's budget (a started promise's first poll belongs
/// to the code that started it).
pub fn unconstrained<F: Future>(f: F) -> impl Future<Output = F::Output> {
    Unconstrained(tokio::task::unconstrained(f))
}

struct Unconstrained<F>(tokio::task::Unconstrained<F>);

impl<F: Future> Future for Unconstrained<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: structural pinning of the only field.
        let inner = unsafe { self.map_unchecked_mut(|s| &mut s.0) };
        velt_coop::unconstrained(|| inner.poll(cx))
    }
}

/// Yields to the scheduler: the task runs again after the other ready tasks had a turn.
pub async fn yield_now() {
    if velt_here() {
        YieldNow(false).await
    } else {
        tokio::task::yield_now().await
    }
}

/// Pending once (after waking itself), then ready.
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
