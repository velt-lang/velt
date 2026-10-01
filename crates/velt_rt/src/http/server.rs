//! HTTP server: `serve(addr, handler)` binds, starts an accept-loop task and returns a server
//! handle (`port`, `close()`); every connection is a tokio task and every request runs a compiled
//! async handler.
//!
//! Generated code describes the handler with a `VeltHandler`: an `init` function that writes the
//! initial handler state for one request (taking ownership of the `VeltReq`), plus the state
//! machine's `poll`/`drop` and state size. The runtime stores that state inline in the request
//! future (size classes, like `spawn`), so a request costs no allocation beyond hyper's own and the
//! request/response objects. The request body is read completely before the handler starts, so
//! `req.body` is a plain synchronous accessor. The handler's result (state offset 0) is a
//! `VeltResp*`.
//!
//! `serve_tls` terminates TLS first (rustls, ALPN h2/http1.1). HTTP/1.1 connections support
//! upgrades: a request asking for one parks its `OnUpgrade` (`upgrade.rs`) for std/websocket.

use super::handler::Shared;
pub use super::handler::{InitFn, VeltHandler};
use super::request::ReqObj;
use super::response::RespObj;
use super::upgrade;
use crate::handle::Handle;
use crate::net::tcp::text_arg;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::compiled::{with_state_store, Compiled, OwnedStore};
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::Bytes;
use http_body_util::Full;
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
        let req = Handle::from_box(Box::new(req));
        let (size, align) = (d.state_size as usize, d.state_align as usize);
        let inner = Compiled::<S>::with_init(d.poll, d.drop, size, align, |st| {
            // SAFETY: generated init writes a fresh state; ownership of `req` moves to it.
            unsafe { (d.init)(d.env, req.ptr() as *mut ReqObj, st) }
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
unsafe fn take_response<S: OwnedStore>(inner: Pin<&mut Compiled<S>>) -> Handle<RespObj> {
    *(inner.state_ptr() as *const Handle<RespObj>)
}

impl<S: OwnedStore> Future for HandlerFut<S> {
    type Output = Response<Full<Bytes>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(status_only(StatusCode::INTERNAL_SERVER_ERROR));
        };
        if inner.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        // SAFETY: the handler completed just now; its result is an owned `VeltResp*` or null.
        let resp = unsafe { take_response(inner.as_mut()) };
        self.inner = None;
        Poll::Ready(if resp.is_null() {
            status_only(StatusCode::INTERNAL_SERVER_ERROR)
        } else {
            // SAFETY: ownership of the response moves back to the runtime.
            unsafe { *resp.into_box() }
        })
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
    if !resp.is_null() {
        // SAFETY: an owned response nobody will send.
        drop(unsafe { resp.into_box() });
    }
}

fn status_only(status: StatusCode) -> Response<Full<Bytes>> {
    let reason = status.canonical_reason().unwrap_or("");
    let mut r = Response::new(Full::new(Bytes::from_static(reason.as_bytes())));
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
    mut req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let parked = ParkedUpgrade(if req.headers().contains_key(UPGRADE) {
        upgrade::park(hyper::upgrade::on(&mut req))
    } else {
        0
    });
    let resp = match ReqObj::read(req, parked.0).await {
        Some(req) => HandlerFut::<S>::new(&shared, req).await,
        None => status_only(StatusCode::BAD_REQUEST),
    };
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
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            // Per-connection failures and resource exhaustion (EMFILE): back off briefly, go on.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let (shared, watcher, tls) = (shared.clone(), graceful.watcher(), tls.clone());
        // Connection errors (client hung up mid-request, failed TLS handshake...) only end that
        // connection.
        tokio::spawn(async move {
            match tls {
                None => serve_connection::<S, _>(TokioIo::new(stream), shared, watcher).await,
                Some(acceptor) => {
                    if let Ok(Ok(s)) =
                        tokio::time::timeout(TLS_HANDSHAKE, acceptor.accept(stream)).await
                    {
                        serve_connection::<S, _>(TokioIo::new(s), shared, watcher).await;
                    }
                }
            }
        });
    }
}

/// Serves one connection (HTTP/1.1 with upgrades, or HTTP/2) until it closes.
async fn serve_connection<S, I>(io: I, shared: Arc<Shared>, watcher: Watcher)
where
    S: OwnedStore,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = hyper::service::service_fn(move |req| handle::<S>(shared.clone(), req));
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
        IoResult::from_io(r, |s| Handle::from_box(Box::new(s)))
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
/// certificate chain and private key; ALPN h2 + http/1.1). Bad PEM fails with `EINVAL`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_serve_tls(
    addr: *const VeltStr,
    handler: *const VeltHandler,
    cert_pem: *const VeltStr,
    key_pem: *const VeltStr,
) -> *mut VeltFut {
    match crate::tls::server_config((*cert_pem).as_bytes(), (*key_pem).as_bytes()) {
        Ok(config) => start(text_arg(addr), *handler, Some(TlsAcceptor::from(config))),
        Err(msg) => new_leaf(async move {
            IoResult::<Handle<ServerObj>>::err(VeltErr::new(code::INVALID_INPUT, &msg))
        }),
    }
}

/// `server.port`: the bound port.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_port(s: Handle<ServerObj>) -> u32 {
    s.obj().port as u32
}

/// `server.close()`: stop accepting, let open connections finish their in-flight requests, and
/// free the handle. The handler's environment is released once the last request finished.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_close(s: Handle<ServerObj>) {
    stop(*s.into_box());
}

/// `await server.shutdown()`: `velt_rt_http_server_close`, then resolves (result `()`) once every
/// in-flight request has finished and the handler's environment was released.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_shutdown(s: Handle<ServerObj>) -> *mut VeltFut {
    let mut released = stop(*s.into_box());
    new_leaf(async move { while released.changed().await.is_ok() {} })
}

fn stop(s: ServerObj) -> watch::Receiver<()> {
    s.accept_loop.abort();
    crate::task::runtime::keep_alive_release();
    s.released
}

/// Dropping a `Server` value: free the handle but keep serving (like Node, a listening server runs
/// until `close()` or process exit, whether or not the program still holds it).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_server_detach(s: Handle<ServerObj>) {
    drop(s.into_box());
}
