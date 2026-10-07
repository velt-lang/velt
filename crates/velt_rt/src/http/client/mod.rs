//! The HTTP client behind the global `fetch` (std/fetch.vlt, docs/std/fetch.md): hyper-util's
//! pooled client over `http://` and `https://` (rustls, `tls.rs`; HTTP/2 when the server offers
//! it through ALPN), redirects followed as the Fetch standard says (`send.rs`), and an
//! `AbortSignal` that cancels the request: the request (connecting, sending, waiting for the
//! response or receiving its body) is dropped the moment the signal is aborted, which closes its
//! connection.
//!
//! `velt_rt_http_fetch_send` completes once the status and headers have arrived, like JS's
//! `fetch`; the body is received when `text()` or `bytes()` asks for it, or chunk by chunk
//! (`res.body`; `body.rs`), decoded as its `content-encoding` says (`decode.rs`). A response is
//! a key into a registry (`crate::registry`): using it after it was released (or a forged key) is
//! a clear runtime error, never a read of freed memory.

mod body;
mod decode;
mod send;
mod target;
#[cfg(test)]
mod tests;

use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use crate::task::abort::{signal_of, Signal};
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::Bytes;
use futures_util::future::Either;
use hyper::header::{HeaderMap, CONTENT_ENCODING, CONTENT_LENGTH};
use hyper::Method;
use std::future::Future;
use std::sync::{Arc, Mutex};

use send::Outgoing;

/// What `fetch` does with a redirect (`RequestInit.redirect`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Redirect {
    /// Follow it (the default), up to 20 times.
    Follow,
    /// Fail.
    Error,
    /// Return the redirect response itself.
    Manual,
}

impl Redirect {
    fn from_abi(mode: u32) -> Redirect {
        match mode {
            1 => Redirect::Error,
            2 => Redirect::Manual,
            _ => Redirect::Follow,
        }
    }
}

/// A response whose head has arrived (`VeltFetchResp` in the ABI docs).
pub struct FetchResp {
    status: u16,
    /// Usually the standard phrase, which needs no allocation.
    status_text: std::borrow::Cow<'static, str>,
    url: String,
    redirected: bool,
    /// The `content-length`, if the server sent one.
    len: Option<u64>,
    /// Taken by the first `velt_rt_http_fetch_resp_headers`.
    headers: Mutex<HeaderMap>,
    /// Taken by a body read (`text()`, `bytes()` until the end; a chunk read until it returns).
    body: Mutex<Option<body::Reader>>,
    /// The request's signal: aborting it also stops receiving the body.
    signal: Option<Arc<Signal>>,
}

/// A fetch response handle (a registry key).
pub type FetchRespHandle = Key<FetchResp>;

static RESPONSES: Registry<FetchResp> = Registry::new();

/// The response behind `r`, or a fatal error when it was already released.
fn obj(r: FetchRespHandle) -> Arc<FetchResp> {
    RESPONSES
        .get(r)
        .unwrap_or_else(|| crate::panic::fatal("a fetch response was used after it was released"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The error of an operation that `signal` aborted (std turns it into the signal's
/// `AbortError` or `TimeoutError`).
fn aborted() -> VeltErr {
    VeltErr::new(code::OTHER, "This operation was aborted")
}

/// Run `work` unless `signal` is aborted first (then `work` is dropped, cancelling it).
async fn abortable<T>(
    signal: Option<&Signal>,
    work: impl Future<Output = Result<T, VeltErr>>,
) -> Result<T, VeltErr> {
    match signal {
        None => work.await,
        Some(s) => {
            let (stop, work) = (std::pin::pin!(s.aborted()), std::pin::pin!(work));
            match futures_util::future::select(stop, work).await {
                Either::Left(((), _)) => Err(aborted()),
                Either::Right((r, _)) => r,
            }
        }
    }
}

/// Copy the flat header list `[name, value, …]` out of a Velt `string[]`.
unsafe fn header_list(headers: *const VeltStrArray, https: bool) -> Result<HeaderMap, VeltErr> {
    let a = &*headers;
    let items: Vec<&[u8]> = (0..a.len as usize)
        .map(|i| (*a.ptr.add(i)).as_bytes())
        .collect();
    send::header_map(&items, https)
}

async fn run(
    ca: Vec<u8>,
    o: Result<Outgoing, VeltErr>,
    mode: Redirect,
    signal: Option<Arc<Signal>>,
) -> Result<FetchResp, VeltErr> {
    let client = send::client(&ca)?;
    let o = o?;
    // A redirect keeps HEAD a HEAD (and nothing becomes one).
    let head = o.method == Method::HEAD || o.method == Method::CONNECT;
    let r = abortable(signal.as_deref(), send::send(&client, o, mode)).await?;
    let (parts, body) = r.response.into_parts();
    let status_text = parts
        .extensions
        .get::<hyper::ext::ReasonPhrase>()
        .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned().into())
        .or_else(|| parts.status.canonical_reason().map(Into::into))
        .unwrap_or_default();
    let len = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    // A response without a body has nothing to decode, whatever it says (undici too).
    let bodiless = head || matches!(parts.status.as_u16(), 101 | 204 | 205 | 304);
    let encoding = (!bodiless)
        .then(|| parts.headers.get(CONTENT_ENCODING))
        .flatten()
        .and_then(|v| v.to_str().ok());
    let reader = body::Reader {
        incoming: body,
        decoder: decode::Decoder::for_encoding(encoding),
        done: false,
        rest: Bytes::new(),
    };
    Ok(FetchResp {
        len,
        status: parts.status.as_u16(),
        status_text,
        url: r.url,
        redirected: r.redirected,
        headers: Mutex::new(parts.headers),
        body: Mutex::new(Some(reader)),
        signal,
    })
}

/// `fetch(url, init)` → result slot `IoResult<VeltFetchResp>`, ready once the response's status
/// and headers have arrived. `headers` is the flat list `[name, value, …]` (copied); the body
/// is `text` (`kind` 1, taken without a copy: the caller's value is left empty), `bytes` (`kind`
/// 2, copied), or none (`kind` 0). `redirect`: 0 follow, 1 error, 2 manual. `signal`: an abort
/// signal handle or 0. `ca`: extra trusted PEM CA certificates ("" = none).
///
/// # Safety
/// The pointers must be valid Velt values; a nonzero `signal` a live signal handle.
#[no_mangle]
#[allow(clippy::too_many_arguments)] // one C call per `fetch`: every option travels at once
pub unsafe extern "C" fn velt_rt_http_fetch_send(
    method: *const VeltStr,
    url: *const VeltStr,
    headers: *const VeltStrArray,
    kind: u32,
    text: *mut VeltStr,
    bytes: *mut VeltBytes,
    redirect: u32,
    signal: u64,
    ca: *const VeltStr,
) -> *mut VeltFut {
    let body = match kind {
        1 => super::take_text(text),
        // Copied: a `u8[]` the caller borrowed is still freed by its owner.
        2 => Bytes::copy_from_slice((*bytes).as_bytes()),
        _ => Bytes::new(),
    };
    let outgoing = (|| {
        let method = Method::from_bytes((*method).as_bytes())
            .map_err(|_| send::invalid("invalid HTTP method"))?;
        let url = target::Target::parse(&(*url).text_lossy())?;
        let headers = header_list(headers, url.is_https())?;
        Ok(Outgoing {
            method,
            url,
            headers,
            body,
        })
    })();
    let ca = (*ca).as_bytes().to_vec();
    let signal = signal_of(signal);
    let mode = Redirect::from_abi(redirect);
    new_leaf(async move {
        match run(ca, outgoing, mode, signal).await {
            Ok(r) => IoResult::ok(RESPONSES.insert(r)),
            Err(e) => IoResult::err(e),
        }
    })
}

/// The status code.
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_status(r: FetchRespHandle) -> u32 {
    obj(r).status as u32
}

/// The status message (`OK`, `Not Found`): the server's reason phrase, else the standard one.
///
/// # Safety
/// `out` receives an owned string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_status_text(
    r: FetchRespHandle,
    out: *mut VeltStr,
) {
    out.write(VeltStr::from_text(&obj(r).status_text));
}

/// The final URL (after redirects).
///
/// # Safety
/// `out` receives an owned string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_url(r: FetchRespHandle, out: *mut VeltStr) {
    out.write(VeltStr::from_text(&obj(r).url));
}

/// Whether a redirect led to the response.
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_redirected(r: FetchRespHandle) -> bool {
    obj(r).redirected
}

/// The headers as the flat list `[name, value, …]` (lowercase names, received order; values
/// that are not UTF-8 decoded lossily). The first call takes them; later calls return `[]`.
///
/// # Safety
/// `out` receives an owned array.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_headers(
    r: FetchRespHandle,
    out: *mut VeltStrArray,
) {
    let headers = std::mem::take(&mut *lock(&obj(r).headers));
    let mut flat = Vec::with_capacity(headers.len() * 2);
    for (name, value) in headers.iter() {
        flat.push(VeltStr::from_text(name.as_str()));
        flat.push(VeltStr::from_text(&String::from_utf8_lossy(
            value.as_bytes(),
        )));
    }
    out.write(VeltStrArray::from_vec(flat));
}

/// Receive the whole body (each response's body is received once: a second read is `EINVAL`).
async fn receive(r: Arc<FetchResp>) -> Result<body::Whole, VeltErr> {
    let Some(reader) = lock(&r.body).take() else {
        return Err(VeltErr::new(
            code::INVALID_INPUT,
            "Body is unusable: Body has already been read",
        ));
    };
    abortable(r.signal.as_deref(), reader.read_all(r.len)).await
}

/// `await res.text()` → result slot `IoResult<VeltStr>`: the body as UTF-8, invalid bytes
/// decoded to U+FFFD (as JS does).
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_text(r: FetchRespHandle) -> *mut VeltFut {
    let r = obj(r);
    new_leaf(async move {
        match receive(r).await {
            Ok(w) => {
                let bytes = w.as_slice();
                // A leading byte order mark is not text (JS's UTF-8 decode drops it too).
                let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
                IoResult::ok(VeltStr::from_text(&String::from_utf8_lossy(bytes)))
            }
            Err(e) => IoResult::<VeltStr>::err(e),
        }
    })
}

/// `await res.bytes()` → result slot `IoResult<VeltBytes>` (the received buffer, not copied
/// again).
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_bytes(r: FetchRespHandle) -> *mut VeltFut {
    let r = obj(r);
    new_leaf(async move {
        match receive(r).await {
            Ok(w) => IoResult::ok(VeltBytes::from_vec(w.into_vec())),
            Err(e) => IoResult::<VeltBytes>::err(e),
        }
    })
}

/// `res.body`'s next chunk → result slot `IoResult<VeltBytes>`: the next decoded bytes, never
/// empty; an empty array once the body is complete (and on every read after that).
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_chunk(r: FetchRespHandle) -> *mut VeltFut {
    let r = obj(r);
    new_leaf(async move {
        let Some(mut reader) = lock(&r.body).take() else {
            return IoResult::ok(VeltBytes::from_vec(vec![]));
        };
        match abortable(r.signal.as_deref(), reader.next()).await {
            Ok(chunk) => {
                if !reader.done {
                    *lock(&r.body) = Some(reader);
                }
                IoResult::ok(VeltBytes::from_vec(chunk.unwrap_or_default()))
            }
            Err(e) => IoResult::<VeltBytes>::err(e),
        }
    })
}

/// Free a fetch response (a released key is ignored); an unread body is dropped, which closes
/// its connection unless it was already complete.
#[no_mangle]
pub extern "C" fn velt_rt_http_fetch_resp_drop(r: FetchRespHandle) {
    drop(RESPONSES.remove(r));
}
