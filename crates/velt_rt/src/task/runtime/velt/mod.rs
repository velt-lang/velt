//! The Velt backend (`VELT_SCHEDULER=velt`, experimental): the runtime's own executor, on `mio`.
//! See [`worker`] for how work is placed and spread, [`timer`] for timers, [`io`] for sockets
//! and [`bridge`] for what still runs on tokio.

mod bridge;
pub(crate) mod coop;
pub(crate) mod io;
mod monitor;
mod queue;
mod spread;
mod task;
#[cfg(test)]
mod tests;
pub(super) mod timer;
mod worker;

pub use task::{AbortHandle, JoinHandle};

use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, Once};
use std::task::{Context, Poll, Wake, Waker};

static STARTED: Once = Once::new();

pub(crate) fn start() {
    STARTED.call_once(|| worker::start(super::worker_count()));
}

/// Spawns `fut` on worker `pinned`, else on this thread's worker (off the workers: worker 0).
pub(super) fn spawn<F>(fut: F, pinned: Option<usize>) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    start();
    let home = pinned.or_else(worker::current).unwrap_or(0);
    let (task, handle) = task::make(fut, home, pinned.is_some());
    worker::schedule_new(task);
    handle
}

/// The workers running: the configured ones, and the spare ones started so far.
pub(super) fn worker_count() -> usize {
    start();
    worker::started()
}

/// Tasks queued for a worker by other threads and not taken yet (quiet.rs).
pub(super) fn queued_tasks() -> usize {
    worker::queued_from_outside()
}

pub(super) fn alive_tasks() -> usize {
    task::ALIVE.load(Ordering::Relaxed)
}

/// The bridge's tokio handle (blocking work, `enter`).
pub(super) fn bridge() -> &'static tokio::runtime::Handle {
    bridge::handle()
}

/// Wakes a thread blocked in [`block_on`].
struct Unpark {
    woken: Mutex<bool>,
    cv: Condvar,
}

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        *self.woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_one();
    }
}

/// Runs `f` on this thread (not a worker), inside the bridge's context.
pub(super) fn block_on<F: Future>(f: F) -> F::Output {
    start();
    coop::driving(|| drive(f))
}

fn drive<F: Future>(f: F) -> F::Output {
    let _bridge = bridge::handle().enter();
    let unpark = Arc::new(Unpark {
        woken: Mutex::new(false),
        cv: Condvar::new(),
    });
    let waker = Waker::from(unpark.clone());
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        coop::refill();
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        let mut woken = unpark.woken.lock().unwrap_or_else(|e| e.into_inner());
        while !*woken {
            woken = unpark.cv.wait(woken).unwrap_or_else(|e| e.into_inner());
        }
        *woken = false;
    }
}
