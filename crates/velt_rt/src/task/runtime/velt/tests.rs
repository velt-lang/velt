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

#[test]
fn a_wake_from_another_thread_reaches_a_waiting_task() {
    let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
    let h = spawn(async move { rx.await.ok() }, None);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        let _ = tx.send(7);
    });
    assert_eq!(block_on(h).ok().flatten(), Some(7));
}

#[test]
fn a_worker_busy_with_waiting_tasks_shares_them() {
    if super::super::worker_count() < 2 {
        return;
    }
    // 200 tasks of ~1 ms of work each, all made on one worker: it is busy long past the limit
    // with tasks waiting, so a helper takes some (checked every few tasks, so a moment when the
    // other workers are busy with other tests does not decide it).
    let ran_on = Arc::new(Mutex::new(HashSet::new()));
    let seen = ran_on.clone();
    let done = spawn(
        async move {
            let hs: Vec<_> = (0..200)
                .map(|_| {
                    let seen = seen.clone();
                    spawn(
                        async move {
                            let t = Instant::now();
                            while t.elapsed() < Duration::from_millis(1) {
                                std::hint::spin_loop();
                            }
                            seen.lock().unwrap().insert(worker::current());
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
                match c.try_read_buf(&mut buf) {
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
