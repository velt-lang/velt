//! The handle a spawn on the runtime's host gives back: [`JoinHandle`] (await the task's output;
//! dropping it detaches the task, whose output is then dropped when it finishes).

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Why a task gave no output.
pub enum JoinError {
    /// The task panicked; the payload, to resume unwinding with.
    Panic(Box<dyn Any + Send>),
    /// The task was cancelled (the host dropped it).
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
pub struct JoinHandle<T>(pub(super) tokio::task::JoinHandle<T>);

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0)
            .poll(cx)
            .map_err(JoinError::from)
    }
}
