//! HTTP server: `serve(addr, handler)` binds, starts an accept-loop task and returns a server
//! handle (`port`, `close()`); every connection is a tokio task and every request runs a compiled
//! async handler.
//!
//! Generated code describes the handler with a `VeltHandler`: an `init` function that writes the
//! initial handler state for one request (taking ownership of the `VeltReq`), plus the state
//! machine's `poll`/`drop` and state size. The runtime stores that state inline in the request
//! future (size classes, like `spawn`), so a request costs no allocation beyond hyper's own and the
//! request/response objects. The handler starts once the request's head has arrived and receives
//! the body while it reads it (`req_body.rs`). The handler's result (state offset 0) is a
//! response key (`response.rs`).
//!
//! A server handle is a registry key too (`crate::registry`): after `close()`, `shutdown()` or
//! dropping the `Server`, any copy of the handle (or a forged one) is inert: `port` is 0 and the
//! other operations do nothing.
//!
//! `serve_tls` terminates TLS first (rustls, ALPN h2/http1.1). HTTP/1.1 connections support
//! upgrades: a request asking for one parks its `OnUpgrade` (`upgrade.rs`) for std/websocket.

use super::body::RespBody;
use super::handler::Shared;
pub use super::handler::{InitFn, VeltHandler};
use super::request::{Conn, ReqObj};
use super::response::RespHandle;
use super::upgrade;
use crate::net::tcp::text_arg;
use crate::registry::{Key, Registry};
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::compiled::{with_state_store, Compiled, OwnedStore};
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::Bytes;
use hyper::body::Incoming;
use hyper::header::UPGRADE;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;

/// How long a client may take to complete its TLS handshake.
const TLS_HANDSHAKE: Duration = Duration::from_secs(10);

/// A server handle (a registry key).
pub type ServerHandle = Key<ServerObj>;

static SERVERS: Registry<ServerObj> = Registry::new();

/// A running server (`VeltServer` in the ABI docs).
pub struct ServerObj {
    port: u16,
    accept_loop: AbortHandle,
    /// Signals (by closing) that the handler was released; see `Shared::released`.
    released: watch::Receiver<()>,
}

/// One request: the compiled handler future plus conversion of its result.
///
/// If hyper drops the request before the handler finished (the client went away), the handler
/// is not cancelled: it runs to completion in a task of its own, like a Node handler whose
/// client disconnected, so the cleanup it would do after its next `await` (returning a pooled
/// connection, committing, `close()`) still happens. That is why the state is boxed: an
/// unfinished handler must be able to leave the request's future.
struct HandlerFut<S: OwnedStore> {
    /// `None` once the handler returned.
    inner: Option<Pin<Box<Compiled<S>>>>,
    /// The request's state borrows the handler's environment, which lives as long as `Shared`
    /// (an HTTP/2 stream's task can outlive its connection's).
    shared: Arc<Shared>,
}

impl<S: OwnedStore> HandlerFut<S> {
    fn new(shared: &Arc<Shared>, req: ReqObj) -> Self {
        let d = &shared.handler();
        let req = super::request::register(req);
        let (size, align) = (d.state_size as usize, d.state_align as usize);
        let inner = Compiled::<S>::with_init(d.poll, d.drop, size, align, |st| {
            // SAFETY: generated init writes a fresh state; ownership of the request key (passed
            // in the pointer-sized slot) moves to it.
            unsafe { (d.init)(d.env, req.bits() as usize as *mut ReqObj, st) }
        });
        HandlerFut {
            inner: Some(Box::pin(inner)),
            shared: shared.clone(),
        }
    }
}

/// Takes the handler's result (state offset 0) once its poll returned `Ready`.
///
/// # Safety
/// The handler must have completed and its result must not have been taken yet.
unsafe fn take_response<S: OwnedStore>(inner: Pin<&mut Compiled<S>>) -> RespHandle {
    *(inner.state_ptr() as *const RespHandle)
}

impl<S: OwnedStore> Future for HandlerFut<S> {
    type Output = Response<RespBody>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(status_only(StatusCode::INTERNAL_SERVER_ERROR));
        };
        if inner.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        // SAFETY: the handler completed just now; its result is a response key (0 = none).
        let resp = unsafe { take_response(inner.as_mut()) };
        self.inner = None;
        // Ownership of the response moves back to the runtime; a dead key is a 500.
        Poll::Ready(
            super::response::take(resp)
                .unwrap_or_else(|| status_only(StatusCode::INTERNAL_SERVER_ERROR)),
        )
    }
}

impl<S: OwnedStore> Drop for HandlerFut<S> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let shared = self.shared.clone();
            crate::task::runtime::handle().spawn(finish_detached(inner, shared));
        }
    }
}

/// Runs a handler whose request was dropped to completion and discards its response.
async fn finish_detached<S: OwnedStore>(mut inner: Pin<Box<Compiled<S>>>, _shared: Arc<Shared>) {
    inner.as_mut().await;
    // SAFETY: the handler completed; nobody else takes its result.
    let resp = unsafe { take_response(inner.as_mut()) };
    // A response nobody will send.
    drop(super::response::take(resp));
}

fn status_only(status: StatusCode) -> Response<RespBody> {
    let reason = status.canonical_reason().unwrap_or("");
    let mut r = Response::new(RespBody::full(Bytes::from_static(reason.as_bytes())));
    *r.status_mut() = status;
    r
}

/// Drops a request's parked upgrade when its handler finishes (or is cancelled) without having
/// claimed it.
struct ParkedUpgrade(u64);

impl Drop for ParkedUpgrade {
    fn drop(&mut self) {
        upgrade::release(self.0);
    }
}

async fn handle<S: OwnedStore>(
    shared: Arc<Shared>,
    conn: Conn,
    mut req: Request<Incoming>,
) -> Result<Response<RespBody>, Infallible> {
    let parked = ParkedUpgrade(if req.headers().contains_key(UPGRADE) {
        upgrade::park(hyper::upgrade::on(&mut req))
    } else {
        0
    });
    let resp = HandlerFut::<S>::new(&shared, ReqObj::new(req, parked.0, conn)).await;
    drop(parked);
    Ok(resp)
}

async fn accept_loop<S: OwnedStore>(
    listener: TcpListener,
    shared: Arc<Shared>,
    tls: Option<TlsAcceptor>,
) {
    // Dropping this (when `close()` aborts the loop) makes every open connection finish its
    // in-flight requests and close.
    let graceful = GracefulShutdown::new();
    let Some(mut stop) = crate::dev::shutdown::register() else {
        return accept_connections::<S>(&listener, shared, &graceful, tls).await;
    };
    // Dev mode: on SIGTERM stop accepting (the supervisor keeps the socket, so new connections
    // wait for the next version) and let in-flight requests finish before the process exits.
    {
        let accept = std::pin::pin!(accept_connections::<S>(&listener, shared, &graceful, tls));
        futures_util::future::select(accept, std::pin::pin!(stop.requested())).await;
    }
    drop(listener);
    let _ = tokio::time::timeout(crate::dev::shutdown::DRAIN, graceful.shutdown()).await;
    drop(stop);
}

async fn accept_connections<S: OwnedStore>(
    listener: &TcpListener,
    shared: Arc<Shared>,
    graceful: &GracefulShutdown,
    tls: Option<TlsAcceptor>,
) {
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(accepted) => accepted,
            // Per-connection failures and resource exhaustion (EMFILE): back off briefly, go on.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let conn = Conn {
            remote,
            local: stream.local_addr().unwrap_or(remote),
            tls: tls.is_some(),
        };
        let (shared, watcher, tls) = (shared.clone(), graceful.watcher(), tls.clone());
        // Connection errors (client hung up mid-request, failed TLS handshake...) only end that
        // connection.
        tokio::spawn(async move {
            match tls {
                None => serve_connection::<S, _>(TokioIo::new(stream), shared, conn, watcher).await,
                Some(acceptor) => {
                    if let Ok(Ok(s)) =
                        tokio::time::timeout(TLS_HANDSHAKE, acceptor.accept(stream)).await
                    {
                        serve_connection::<S, _>(TokioIo::new(s), shared, conn, watcher).await;
                    }
                }
            }
        });
    }
}

/// Serves one connection (HTTP/1.1 with upgrades, or HTTP/2) until it closes.
async fn serve_connection<S, I>(io: I, shared: Arc<Shared>, conn: Conn, watcher: Watcher)
where
    S: OwnedStore,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = hyper::service::service_fn(move |req| handle::<S>(shared.clone(), conn, req));
    let mut builder = auto::Builder::new(TokioExecutor::new());
    // pipeline_flush: responses to pipelined requests go out in one write instead of one
    // `writev` each (5× on TechEmpower's pipelined plaintext, bench/web/RESULTS.md).
    // half_close: a client that sends its requests and then shuts down its side still gets
    // every response (hyper otherwise drops them, and with pipeline_flush all of them).
    builder.http1().pipeline_flush(true).half_close(true);
    let conn = builder.serve_connection_with_upgrades(io, svc).into_owned();
    let _ = watcher.watch(conn).await;
}

/// Binds and starts the accept loop; `tls` = serve HTTPS.
unsafe fn start(addr: String, handler: VeltHandler, tls: Option<TlsAcceptor>) -> *mut VeltFut {
    let shared = Shared::new(handler);
    new_leaf(async move {
        let r = crate::dev::bind(addr.as_str()).await.and_then(|l| {
            let port = l.local_addr()?.port();
            let desc = shared.handler();
            let released = shared.released();
            // Dev mode: a replacement handler may need any state size, so store states on the
            // heap instead of picking an inline size class from this one.
            let (size, align) = if crate::dev::enabled() {
                (usize::MAX, 1)
            } else {
                (desc.state_size as usize, desc.state_align as usize)
            };
            let task = with_state_store!(size, align, |S| tokio::spawn(accept_loop::<S>(
                l, shared, tls
            )));
            // A listening server keeps the process alive after `async main` (like Node).
            crate::task::runtime::keep_alive_acquire();
            Ok(ServerObj {
                port,
                accept_loop: task.abort_handle(),
                released,
            })
        });
        IoResult::from_io(r, |s| SERVERS.insert(s))
    })
}

/// `serve(addr, handler)` → result slot `IoResult<VeltServer*>` once the address is bound
/// (`"host:port"`, port 0 picks a free port). The server then runs in the background until
/// `velt_rt_http_server_close`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_serve(
    addr: *const VeltStr,
    handler: *const VeltHandler,
) -> *mut VeltFut {
    start(text_arg(addr), *handler, None)
}

/// `serve({ tls: { cert, key } }, handler)`: like `velt_rt_http_serve`, over TLS (PEM
/// certificate chain and private key; ALPN h2 + http/1.1). Bad PEM fails with `EINVAL` (the
/// handler's environment is released then, as on any failed `serve`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_serve_tls(
    addr: *const VeltStr,
    handler: *const VeltHandler,
    cert_pem: *const VeltStr,
    key_pem: *const VeltStr,
) -> *mut VeltFut {
    match crate::tls::server_config(
        (*cert_pem).text_lossy().as_bytes(),
        (*key_pem).text_lossy().as_bytes(),
    ) {
        Ok(config) => start(text_arg(addr), *handler, Some(TlsAcceptor::from(config))),
        Err(msg) => {
            // No server will own the handler: release its environment (captures' drop hooks
            // run) as a failed bind does when its `Shared` is dropped.
            super::handler::release_env((*handler).env);
            new_leaf(async move {
                IoResult::<ServerHandle>::err(VeltErr::new(code::INVALID_INPUT, &msg))
            })
        }
    }
}

/// `server.port`: the bound port (0 once the handle is closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_port(s: ServerHandle) -> u32 {
    SERVERS.get(s).map_or(0, |s| s.port as u32)
}

/// `server.close()`: stop accepting, let open connections finish their in-flight requests, and
/// free the handle. The handler's environment is released once the last request finished. A
/// closed (or forged) handle is ignored.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_close(s: ServerHandle) {
    stop(s);
}

/// `await server.shutdown()`: `velt_rt_http_server_close`, then resolves (result `()`) once every
/// in-flight request has finished and the handler's environment was released (at once for a
/// closed handle).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_shutdown(s: ServerHandle) -> *mut VeltFut {
    let released = stop(s);
    new_leaf(async move {
        if let Some(mut released) = released {
            while released.changed().await.is_ok() {}
        }
    })
}

fn stop(s: ServerHandle) -> Option<watch::Receiver<()>> {
    let s = SERVERS.remove(s)?;
    s.accept_loop.abort();
    crate::task::runtime::keep_alive_release();
    Some(s.released.clone())
}

/// Dropping a `Server` value: free the handle but keep serving (like Node, a listening server runs
/// until `close()` or process exit, whether or not the program still holds it).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_detach(s: ServerHandle) {
    drop(SERVERS.remove(s));
}
