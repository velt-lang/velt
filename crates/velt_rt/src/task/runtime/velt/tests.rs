//! The Velt backend on its own (called directly, whatever `VELT_SCHEDULER` says).

use super::super::handle::JoinError;
use super::{block_on, io, spawn, timer::Sleep, worker};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn tasks_spawned_from_tasks_join_with_their_results() {
    let sum = block_on(spawn(
        async {
            let hs: Vec<_> = (0..100u64)
                .map(|i| spawn(async move { i * 2 }, None))
                .collect();
            let mut sum = 0;
            for h in hs {
                sum += h.await.ok().expect("joined");
            }
            sum
        },
        None,
    ));
    assert_eq!(sum.ok(), Some(9900));
}

#[test]
fn a_panic_reaches_the_join_handle() {
    let r = block_on(spawn(async { panic!("boom") }, None));
    let Err(JoinError::Panic(p)) = r else {
        panic!("expected a panic");
    };
    assert_eq!(p.downcast_ref::<&str>(), Some(&"boom"));
}

#[test]
fn aborts_racing_a_worker_taking_the_task_are_not_lost() {
    // Tasks aborted right after their spawn, while queued: an abort can land as a worker takes
    // the task, and each must still be dropped (the task's state is written by both sides; see
    // the module docs of `task`).
    struct Live(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for Live {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    const N: usize = 100_000;
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(N));
    for _ in 0..N {
        let guard = Live(live.clone());
        let h = spawn(
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            },
            None,
        );
        h.abort_handle().abort();
    }
    let until = Instant::now() + Duration::from_secs(30);
    while live.load(std::sync::atomic::Ordering::SeqCst) > 0 && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(
        live.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "aborted tasks were never dropped"
    );
}

#[test]
fn an_aborted_task_is_dropped_and_reports_cancelled() {
    struct Flag(Arc<Mutex<bool>>);
    impl Drop for Flag {
        fn drop(&mut self) {
            *self.0.lock().unwrap() = true;
        }
    }
    let dropped = Arc::new(Mutex::new(false));
    let flag = Flag(dropped.clone());
    let h = spawn(
        async move {
            let _flag = flag;
            std::future::pending::<()>().await
        },
        None,
    );
    h.abort_handle().abort();
    assert!(matches!(block_on(h), Err(JoinError::Cancelled)));
    assert!(*dropped.lock().unwrap());
}

#[test]
fn a_sleep_never_completes_before_its_deadline() {
    // (The order due timers fire in is `timer::tests`; here a busy worker may poll late, which
    // only delays.)
    let hs: Vec<_> = [30u64, 10, 20]
        .into_iter()
        .map(|ms| {
            spawn(
                async move {
                    let start = Instant::now();
                    Sleep::until(start + Duration::from_millis(ms)).await;
                    start.elapsed()
                },
                None,
            )
        })
        .collect();
    for (h, ms) in hs.into_iter().zip([30u64, 10, 20]) {
        let waited = block_on(h).ok().expect("joined");
        assert!(
            waited >= Duration::from_millis(ms),
            "{ms} ms timer fired after {waited:?}"
        );
    }
}

/// Awaits `rx`, telling `waiting` once it waits (so a test wakes a task that really waits).
async fn await_after_signal<T>(
    rx: tokio::sync::oneshot::Receiver<T>,
    waiting: tokio::sync::oneshot::Sender<()>,
) -> Option<T> {
    let mut rx = std::pin::pin!(rx);
    let mut waiting = Some(waiting);
    std::future::poll_fn(|cx| match std::future::Future::poll(rx.as_mut(), cx) {
        std::task::Poll::Ready(v) => std::task::Poll::Ready(v.ok()),
        std::task::Poll::Pending => {
            if let Some(w) = waiting.take() {
                let _ = w.send(());
            }
            std::task::Poll::Pending
        }
    })
    .await
}

#[test]
fn a_wake_from_another_thread_reaches_a_waiting_task() {
    let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
    let (waiting_tx, waiting) = tokio::sync::oneshot::channel();
    let h = spawn(await_after_signal(rx, waiting_tx), None);
    std::thread::spawn(move || {
        let _ = waiting.blocking_recv();
        let _ = tx.send(7);
    });
    assert_eq!(block_on(h).ok().flatten(), Some(7));
}

#[test]
fn a_worker_busy_with_waiting_tasks_shares_them() {
    if super::super::worker_count() < 2 {
        return;
    }
    // 200 tasks made on one worker, each computing until a task has run on another worker
    // (10 s at most in all): the worker stays busy with tasks waiting until a helper takes some,
    // however long other tests keep the workers busy first.
    let ran_on = Arc::new(Mutex::new(HashSet::new()));
    let seen = ran_on.clone();
    let until = Instant::now() + Duration::from_secs(10);
    let done = spawn(
        async move {
            let hs: Vec<_> = (0..200)
                .map(|_| {
                    let seen = seen.clone();
                    spawn(
                        async move {
                            seen.lock().unwrap().insert(worker::current());
                            while seen.lock().unwrap().len() < 2 && Instant::now() < until {
                                std::hint::spin_loop();
                            }
                        },
                        None,
                    )
                })
                .collect();
            for h in hs {
                let _ = h.await;
            }
        },
        Some(0),
    );
    let _ = block_on(done);
    assert!(
        ran_on.lock().unwrap().len() > 1,
        "all work stayed on one worker"
    );
}

#[test]
fn tasks_queued_behind_a_long_poll_run_on_another_worker() {
    if super::super::worker_count() < 2 {
        return;
    }
    // Tasks spawned by a task that then computes without returning until they all ran (10 s at
    // most): they are queued on its worker, which makes no progress, so the monitor moves them
    // to another worker.
    let ran_late = Arc::new(Mutex::new(Vec::new()));
    let seen = ran_late.clone();
    let done = spawn(
        async move {
            let computing = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let hs: Vec<_> = (0..8)
                .map(|_| {
                    let (seen, computing) = (seen.clone(), computing.clone());
                    spawn(
                        async move {
                            let late = !computing.load(std::sync::atomic::Ordering::SeqCst);
                            seen.lock().unwrap().push(late);
                        },
                        None,
                    )
                })
                .collect();
            let t = Instant::now();
            while seen.lock().unwrap().len() < 8 && t.elapsed() < Duration::from_secs(10) {
                std::hint::spin_loop();
            }
            computing.store(false, std::sync::atomic::Ordering::SeqCst);
            for h in hs {
                let _ = h.await;
            }
        },
        None,
    );
    let _ = block_on(done);
    let ran_late = ran_late.lock().unwrap();
    assert_eq!(ran_late.len(), 8);
    assert!(
        ran_late.iter().all(|late| !late),
        "tasks waited for the long poll: {ran_late:?}"
    );
}

#[test]
fn a_tcp_echo_runs_on_the_backends_own_sockets() {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = std_listener.local_addr().unwrap();
    let echoed = block_on(spawn(
        async move {
            let listener = io::TcpListener::from_std(std_listener).expect("listener");
            let server = spawn(
                async move {
                    let (mut s, _) = listener.accept().await.expect("accept");
                    let mut buf = [0u8; 5];
                    s.read_exact(&mut buf).await.expect("read");
                    s.write_all(&buf).await.expect("write");
                },
                None,
            );
            let mut c = io::TcpStream::connect(addr).await.expect("connect");
            c.write_all(b"hello").await.expect("write");
            let mut back = [0u8; 5];
            c.read_exact(&mut back).await.expect("read");
            let _ = server.await;
            back
        },
        None,
    ));
    assert_eq!(&echoed.ok().expect("joined"), b"hello");
}

#[test]
fn data_after_a_short_poll_read_still_wakes_the_reader() {
    // `AsyncRead` (a driver's framed reads) clears the readiness after a short read too.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = std_listener.local_addr().unwrap();
    let got = block_on(spawn(
        async move {
            let listener = io::TcpListener::from_std(std_listener).expect("listener");
            let writer = spawn(
                async move {
                    let (mut s, _) = listener.accept().await.expect("accept");
                    for part in [&b"one"[..], b"two", b"three"] {
                        s.write_all(part).await.expect("write");
                        Sleep::until(Instant::now() + Duration::from_millis(30)).await;
                    }
                },
                None,
            );
            let mut c = io::TcpStream::connect(addr).await.expect("connect");
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while got.len() < 11 {
                let n = c.read(&mut buf).await.expect("read");
                assert!(n > 0, "end of stream after {got:?}");
                got.extend_from_slice(&buf[..n]);
            }
            let _ = writer.await;
            got
        },
        None,
    ));
    assert_eq!(got.ok().expect("joined"), b"onetwothree");
}

#[test]
fn data_after_a_short_read_still_wakes_the_reader() {
    // A short read clears the readiness without a `WouldBlock`: what arrives later must bring
    // an event that wakes the next wait.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = std_listener.local_addr().unwrap();
    let got = block_on(spawn(
        async move {
            let listener = io::TcpListener::from_std(std_listener).expect("listener");
            let writer = spawn(
                async move {
                    let (mut s, _) = listener.accept().await.expect("accept");
                    s.write_all(b"one").await.expect("write");
                    Sleep::until(Instant::now() + Duration::from_millis(30)).await;
                    s.write_all(b"two").await.expect("write");
                    Sleep::until(Instant::now() + Duration::from_millis(30)).await;
                },
                None,
            );
            let c = io::TcpStream::connect(addr).await.expect("connect");
            let mut got = Vec::new();
            while got.len() < 6 {
                c.readable().await.expect("readable");
                let mut buf = Vec::with_capacity(64);
                match c.try_read_buf(&mut buf, 64) {
                    Ok(_) => got.extend_from_slice(&buf),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("read: {e}"),
                }
            }
            let _ = writer.await;
            got
        },
        None,
    ));
    assert_eq!(got.ok().expect("joined"), b"onetwo");
}

/// Tells the child process of [`a_task_that_runs_on_holds_back_no_other_tasks_timer`] to play
/// its part.
const SPIN_CHILD: &str = "VELT_RT_SPIN_CHILD";

#[test]
fn a_task_that_runs_on_holds_back_no_other_tasks_timer() {
    // With one worker and with two: a task that computes without returning, queued behind a task
    // sleeping on the same worker, must not keep that sleep from ending (the spare worker takes
    // over when no other can). Each count runs in a child process, as the runtime starts once.
    for workers in ["1", "2"] {
        let exe = std::env::current_exe().expect("test executable");
        let out = crate::abi_tests::command::command(exe)
            .args([
                "--exact",
                "task::runtime::velt::tests::spin_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SPIN_CHILD, "1")
            .env(super::super::THREADS_ENV, workers)
            .output()
            .expect("run the child");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{workers} worker(s): {text}");
        assert!(text.contains("1 passed"), "{workers} worker(s): {text}");
    }
}

#[test]
fn spin_child() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if std::env::var_os(SPIN_CHILD).is_none() {
        return;
    }
    let flag = Arc::new(AtomicBool::new(false));
    let seen = flag.clone();
    let ended = block_on(spawn(
        async move {
            // Queued behind this task on its worker: it runs once this task sleeps.
            let spinner = spawn(
                async move {
                    let t = Instant::now();
                    while !seen.load(Ordering::SeqCst) {
                        if t.elapsed() > Duration::from_secs(10) {
                            return false;
                        }
                        std::hint::spin_loop();
                    }
                    true
                },
                None,
            );
            Sleep::until(Instant::now() + Duration::from_millis(20)).await;
            flag.store(true, Ordering::SeqCst);
            spinner.await.ok()
        },
        None,
    ));
    assert_eq!(
        ended.ok().flatten(),
        Some(true),
        "the sleep waited for the spin"
    );
}
