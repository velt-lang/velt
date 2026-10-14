//! The handles a spawn gives back, the same for every backend: [`JoinHandle`] (await the task's
//! output; dropping it detaches the task, whose output is then dropped when it finishes) and
//! [`AbortHandle`] (drop the task's future on its next turn).

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Why a task gave no output.
pub enum JoinError {
    /// The task panicked; the payload, to resume unwinding with.
    Panic(Box<dyn Any + Send>),
    /// The task was aborted.
    Cancelled,
}

impl JoinError {
    pub fn is_panic(&self) -> bool {
        matches!(self, JoinError::Panic(_))
    }

    /// The panic payload (call only when [`JoinError::is_panic`]).
    pub fn into_panic(self) -> Box<dyn Any + Send> {
        match self {
            JoinError::Panic(p) => p,
            JoinError::Cancelled => Box::new("task was cancelled"),
        }
    }
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Panic(_) => f.write_str("task panicked"),
            JoinError::Cancelled => f.write_str("task was cancelled"),
        }
    }
}

impl From<tokio::task::JoinError> for JoinError {
    fn from(e: tokio::task::JoinError) -> Self {
        if e.is_panic() {
            JoinError::Panic(e.into_panic())
        } else {
            JoinError::Cancelled
        }
    }
}

/// A spawned task's output, once it finishes.
pub enum JoinHandle<T> {
    Tokio(tokio::task::JoinHandle<T>),
    Velt(super::velt::JoinHandle<T>),
}

impl<T> JoinHandle<T> {
    /// Whether the task finished (its output, or why there is none, is ready).
    pub fn is_finished(&self) -> bool {
        match self {
            JoinHandle::Tokio(h) => h.is_finished(),
            JoinHandle::Velt(h) => h.is_finished(),
        }
    }

    /// Aborts the task (see [`AbortHandle`]).
    pub fn abort(&self) {
        self.abort_handle().abort()
    }

    /// A handle that aborts the task.
    pub fn abort_handle(&self) -> AbortHandle {
        match self {
            JoinHandle::Tokio(h) => AbortHandle::Tokio(h.abort_handle()),
            JoinHandle::Velt(h) => AbortHandle::Velt(h.abort_handle()),
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            JoinHandle::Tokio(h) => Pin::new(h).poll(cx).map_err(JoinError::from),
            JoinHandle::Velt(h) => Pin::new(h).poll(cx),
        }
    }
}

/// Aborts a task: its future is dropped on its next turn, and its join handle reports
/// [`JoinError::Cancelled`].
pub enum AbortHandle {
    Tokio(tokio::task::AbortHandle),
    Velt(super::velt::AbortHandle),
}

impl AbortHandle {
    pub fn abort(&self) {
        match self {
            AbortHandle::Tokio(h) => h.abort(),
            AbortHandle::Velt(h) => h.abort(),
        }
    }
}
