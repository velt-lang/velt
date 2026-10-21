//! A task of the Velt backend: its future, its state and its home worker, and the join and abort
//! handles a spawn gives back.
//!
//! State: `IDLE` (waiting for a wake), `SCHEDULED` (in a queue), `RUNNING` (being polled),
//! `NOTIFIED` (woken while running: queued again once the poll returns), `DONE`. A wake moves
//! `IDLE` to `SCHEDULED` and queues the task on its home worker; only the worker that took it
//! from a queue touches the future, so the future needs no lock.

use super::super::handle::JoinError;
use futures_util::task::{ArcWake, AtomicWaker};
use std::cell::UnsafeCell;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};

const IDLE: u8 = 0;
const SCHEDULED: u8 = 1;
const RUNNING: u8 = 2;
const NOTIFIED: u8 = 3;
const DONE: u8 = 4;

/// Tasks spawned and not finished (for the exit path's `alive_tasks`).
pub(super) static ALIVE: AtomicUsize = AtomicUsize::new(0);

type BoxFut = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(super) struct Task {
    state: AtomicU8,
    /// The worker whose queue a wake puts the task on.
    pub(super) home: AtomicUsize,
    /// Placed on its worker by the program (`spawn(p, { worker })`): never offered to another.
    pub(super) pinned: bool,
    aborted: AtomicBool,
    fut: UnsafeCell<Option<BoxFut>>,
}

// SAFETY: `fut` is only touched by the one worker that moved the state to RUNNING (or by the
// last owner when the task is dropped).
unsafe impl Sync for Task {}

/// What polling a task gave.
pub(super) enum Ran {
    /// Finished, or aborted.
    Done,
    /// Waiting for a wake.
    Waiting,
    /// Woken while it ran: queue it again (behind the tasks already queued).
    Again,
}

impl Task {
    pub(super) fn new(fut: BoxFut, home: usize, pinned: bool) -> Arc<Task> {
        ALIVE.fetch_add(1, Ordering::Relaxed);
        Arc::new(Task {
            state: AtomicU8::new(SCHEDULED),
            home: AtomicUsize::new(home),
            pinned,
            aborted: AtomicBool::new(false),
            fut: UnsafeCell::new(Some(fut)),
        })
    }

    /// Polls the task once (it was taken from a queue: state SCHEDULED).
    pub(super) fn run(self: &Arc<Self>) -> Ran {
        self.state.store(RUNNING, Ordering::Release);
        // SAFETY: this worker holds the task (RUNNING); nobody else touches `fut`.
        let slot = unsafe { &mut *self.fut.get() };
        let finished = match slot.as_mut() {
            None => true,
            Some(_) if self.aborted.load(Ordering::Acquire) => true,
            Some(fut) => {
                let waker = futures_util::task::waker_ref(self);
                let mut cx = Context::from_waker(&waker);
                super::coop::refill();
                fut.as_mut().poll(&mut cx).is_ready()
            }
        };
        if finished {
            self.finish(slot);
            return Ran::Done;
        }
        match self
            .state
            .compare_exchange(RUNNING, IDLE, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ran::Waiting,
            Err(_) => {
                // NOTIFIED: woken during the poll.
                self.state.store(SCHEDULED, Ordering::Release);
                Ran::Again
            }
        }
    }

    fn finish(&self, slot: &mut Option<BoxFut>) {
        self.state.store(DONE, Ordering::Release);
        if slot.take().is_some() {
            ALIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn abort(self: &Arc<Self>) {
        self.aborted.store(true, Ordering::Release);
        ArcWake::wake_by_ref(self);
    }
}

impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        let mut s = arc_self.state.load(Ordering::Acquire);
        loop {
            let next = match s {
                IDLE => SCHEDULED,
                RUNNING => NOTIFIED,
                _ => return,
            };
            match arc_self
                .state
                .compare_exchange_weak(s, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(now) => s = now,
            }
        }
        if s == IDLE {
            super::worker::schedule(arc_self.clone());
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if self.fut.get_mut().take().is_some() {
            ALIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The output of a task, shared by the task and its join handle.
struct Join<T> {
    done: AtomicBool,
    output: Mutex<Option<Result<T, JoinError>>>,
    waiter: AtomicWaker,
}

impl<T> Join<T> {
    fn set(&self, r: Result<T, JoinError>) {
        *self.output.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        self.done.store(true, Ordering::Release);
        self.waiter.wake();
    }
}

/// The spawned future: runs `fut` and leaves its output (or its panic) in `join`. Dropped
/// before finishing (aborted, or the process ends), it drops `fut` and then leaves `Cancelled`,
/// so whoever awaits the task sees what `fut` owned released.
struct Body<F: Future> {
    fut: std::mem::ManuallyDrop<F>,
    join: Arc<Join<F::Output>>,
}

impl<F: Future> Future for Body<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: structural pinning of `fut`; `join` is never moved out.
        let this = unsafe { self.get_unchecked_mut() };
        let fut = unsafe { Pin::new_unchecked(&mut *this.fut) };
        match catch_unwind(AssertUnwindSafe(|| fut.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(v)) => {
                this.join.set(Ok(v));
                Poll::Ready(())
            }
            Err(p) => {
                this.join.set(Err(JoinError::Panic(p)));
                Poll::Ready(())
            }
        }
    }
}

impl<F: Future> Drop for Body<F> {
    fn drop(&mut self) {
        // SAFETY: dropped once, here; pinned in place (it is never moved out).
        unsafe { std::mem::ManuallyDrop::drop(&mut self.fut) };
        if !self.join.done.load(Ordering::Acquire) {
            self.join.set(Err(JoinError::Cancelled));
        }
    }
}

/// Makes a task of `fut` (not yet queued) and its join handle.
pub(super) fn make<F>(fut: F, home: usize, pinned: bool) -> (Arc<Task>, JoinHandle<F::Output>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let join = Arc::new(Join {
        done: AtomicBool::new(false),
        output: Mutex::new(None),
        waiter: AtomicWaker::new(),
    });
    let body = Body {
        fut: std::mem::ManuallyDrop::new(fut),
        join: join.clone(),
    };
    let task = Task::new(Box::pin(body), home, pinned);
    let handle = JoinHandle {
        join,
        task: Arc::downgrade(&task),
    };
    (task, handle)
}

/// A task's output once it finishes; dropping it detaches the task (its output is dropped when
/// the task finishes).
pub struct JoinHandle<T> {
    join: Arc<Join<T>>,
    task: Weak<Task>,
}

impl<T> JoinHandle<T> {
    pub fn is_finished(&self) -> bool {
        self.join.done.load(Ordering::Acquire)
    }

    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle(self.task.clone())
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let join = &self.join;
        if !join.done.load(Ordering::Acquire) {
            join.waiter.register(cx.waker());
            if !join.done.load(Ordering::Acquire) {
                return Poll::Pending;
            }
        }
        let out = join.output.lock().unwrap_or_else(|e| e.into_inner()).take();
        Poll::Ready(out.unwrap_or(Err(JoinError::Cancelled)))
    }
}

/// Aborts a task: its future is dropped on its next turn.
pub struct AbortHandle(Weak<Task>);

impl AbortHandle {
    pub fn abort(&self) {
        if let Some(task) = self.0.upgrade() {
            task.abort();
        }
    }
}
