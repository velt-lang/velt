//! A build's turn at its output (`Place::lock`): a lock on a file, and a word on stderr when the
//! wait for it is long. Two builds of one output at once are common (an editor and a terminal,
//! tests run in parallel), and most such waits are short: said every time, the line would read as
//! a warning, and fail whatever expects a build to print nothing.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// How long a build waits for its turn before it says so (without `-v`).
pub(crate) const QUIET_WAIT: Duration = Duration::from_secs(1);

/// Lock the file at `path`, waiting for whoever holds it; `say` is called once the wait has
/// lasted `after`, and never when the lock is had sooner. `None` where no lock can be had.
pub(super) fn take(
    path: &Path,
    after: Duration,
    say: impl FnOnce() + Send,
) -> Option<std::fs::File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = (std::fs::File::options().create(true).truncate(false))
        .write(true)
        .open(path)
        .ok()?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            let (taken, waited) = mpsc::channel::<()>();
            let locked = std::thread::scope(|s| {
                s.spawn(move || {
                    if waited.recv_timeout(after) == Err(RecvTimeoutError::Timeout) {
                        say();
                    }
                });
                let locked = file.lock();
                // Ends the wait above before its time is up: the turn came soon enough.
                drop(taken);
                locked
            });
            locked.ok()?;
        }
        Err(_) => return None,
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The lock file held by another handle, as another build holds it.
    fn held(dir: &Path) -> (std::path::PathBuf, std::fs::File) {
        let path = dir.join("out.std.lock");
        let holder = std::fs::File::create(&path).unwrap();
        holder.lock().unwrap();
        (path, holder)
    }

    /// A turn that comes before [`QUIET_WAIT`] is taken without a word.
    #[test]
    fn a_short_wait_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (path, holder) = held(dir.path());
        let said = AtomicBool::new(false);
        let (started, starting) = mpsc::channel();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                started.send(()).unwrap();
                take(&path, QUIET_WAIT, || said.store(true, Ordering::SeqCst))
            });
            starting.recv().unwrap();
            // timing: the waiter is blocked on the lock by now; released well inside QUIET_WAIT.
            std::thread::sleep(Duration::from_millis(100));
            drop(holder);
            assert!(waiter.join().unwrap().is_some(), "no turn");
        });
        assert!(!said.load(Ordering::SeqCst), "a short wait was reported");
    }

    /// A long wait is said once, while it lasts: the holder lets go only after hearing it.
    #[test]
    fn a_long_wait_is_said_while_it_lasts() {
        let dir = tempfile::tempdir().unwrap();
        let (path, holder) = held(dir.path());
        let (said, heard) = mpsc::channel();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                let said = said.clone();
                take(&path, Duration::from_millis(20), move || {
                    said.send(()).unwrap()
                })
            });
            heard
                .recv_timeout(Duration::from_secs(60))
                .expect("never said");
            drop(holder);
            assert!(waiter.join().unwrap().is_some(), "no turn");
        });
        drop(said);
        assert!(heard.try_recv().is_err(), "said more than once");
    }

    /// A free lock is taken at once, without a word even when `after` is zero (`-v`).
    #[test]
    fn a_free_turn_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("free.std.lock");
        let said = AtomicBool::new(false);
        assert!(take(&path, Duration::ZERO, || said.store(true, Ordering::SeqCst)).is_some());
        assert!(!said.load(Ordering::SeqCst));
    }
}
