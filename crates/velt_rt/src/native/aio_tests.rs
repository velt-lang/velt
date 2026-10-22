//! The SDK's async I/O (`velt_native::{rt, net, time}`) against this runtime's table, on the
//! scheduler the test process runs (the Velt backend's side runs in the native-package tests).

use std::io::{Read, Write};
use std::sync::OnceLock;
use std::task::Poll;
use std::time::Duration;

use super::{VeltRtApi, VELT_RT_API};
use crate::result::IoResult;
use crate::task::runtime::{block_on, spawn};
use crate::task::{SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use velt_native as sdk;

/// Hands the SDK this runtime's table (as `velt_native_init_<pkg>` would), once.
fn init() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        // The SDK silences std's panic hook (a library has its own std); this test process shares
        // one, so keep the runtime's.
        let hook = std::panic::take_hook();
        // SAFETY: the runtime's static table, with the SDK's layout (`sdk_layouts_match_the_runtime`).
        let rc =
            unsafe { sdk::__init(&VELT_RT_API as *const VeltRtApi as *const sdk::Api, "test") };
        std::panic::set_hook(hook);
        assert_eq!(rc, 0);
    });
}

fn poll_raw(f: &SendPtr<VeltFut>, cx: &mut std::task::Context<'_>) -> Poll<()> {
    let raw = crate::task::raw_cx(cx);
    // SAFETY: a live future; `raw` outlives the call.
    match unsafe { ((*f.0).poll)(f.0, raw.ptr()) } {
        PENDING => Poll::Pending,
        _ => Poll::Ready(()),
    }
}

/// Awaits a runtime future whose result is `IoResult<u64>`, from a runtime task: the error code,
/// the value and the message.
async fn await_u64(f: SendPtr<VeltFut>) -> (i32, u64, String) {
    std::future::poll_fn(|cx| poll_raw(&f, cx)).await;
    // SAFETY: READY: the slot holds the result, moved out; then the future is freed.
    unsafe {
        let r = ((f.0 as *mut u8).add(FUT_RESULT_OFFSET) as *const IoResult<u64>).read();
        ((*f.0).drop)(f.0);
        let value = if r.err.code == 0 {
            r.value.assume_init()
        } else {
            0
        };
        (r.err.code, value, r.err.message.text_lossy().into_owned())
    }
}

fn run<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
    init();
    match block_on(spawn(fut)) {
        Ok(v) => v,
        Err(_) => panic!("the test task failed"),
    }
}

#[test]
fn an_exported_async_function_runs_on_the_awaiting_task() {
    let r = run(async {
        let f = SendPtr(sdk::rt::__async::<Result<u64, sdk::Error>, _>(async {
            sdk::time::sleep(Duration::from_millis(5)).await;
            Ok(42)
        }) as *mut VeltFut);
        await_u64(f).await
    });
    assert_eq!((r.0, r.1), (0, 42));
}

#[test]
fn a_panic_in_an_exported_async_function_is_its_error() {
    let r = run(async {
        let f = SendPtr(
            sdk::rt::__async::<Result<u64, sdk::Error>, _>(async { panic!("boom") })
                as *mut VeltFut,
        );
        await_u64(f).await
    });
    assert_ne!(r.0, 0);
    assert_eq!(r.2, "native panic: boom");
}

#[test]
fn a_tcp_stream_echoes_through_tokios_traits() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).unwrap();
        s.write_all(&buf).unwrap();
    });
    let echoed = run(async move {
        let mut s = sdk::net::TcpStream::connect(&addr).await.unwrap();
        s.set_nodelay(true).unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).await.unwrap();
        s.shutdown().await.unwrap();
        buf
    });
    server.join().unwrap();
    assert_eq!(&echoed, b"hello");
}

#[test]
fn a_refused_connection_is_an_io_error_of_its_kind() {
    // A port nobody listens on: bind, then close.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    let kind = run(async move {
        match sdk::net::TcpStream::connect(&addr).await {
            Ok(_) => None,
            Err(e) => Some(e.kind()),
        }
    });
    assert_eq!(kind, Some(std::io::ErrorKind::ConnectionRefused));
}

#[test]
fn a_timeout_elapses_and_a_sleep_never_ends_early() {
    let (timed_out, waited) = run(async {
        let start = sdk::time::Instant::now();
        let r = sdk::time::timeout(Duration::from_millis(20), std::future::pending::<()>()).await;
        (r.is_err(), start.elapsed())
    });
    assert!(timed_out);
    assert!(waited >= Duration::from_millis(20), "{waited:?}");
}

#[test]
fn a_spawned_task_runs_and_a_wrapped_waker_wakes_it() {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    let (tx, rx) = std::sync::mpsc::channel();
    run(async move {
        sdk::rt::spawn(async move {
            // `FuturesUnordered` polls with its own wakers: the sleep gets a library waker.
            let mut set = FuturesUnordered::new();
            set.push(sdk::time::sleep(Duration::from_millis(5)));
            set.next().await;
            let _ = tx.send(7);
        });
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).ok(), Some(7));
}
