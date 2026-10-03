//! Minimal HTTP client: `fetch(method, url, headers, body)` over hyper-util's pooled client,
//! `http://` and `https://` (rustls, `tls.rs`; HTTP/2 when the server offers it through ALPN).
//! The response body is read fully before the future completes, so the resulting
//! `VeltFetchResp*` accessors are synchronous. A response is a key into a registry
//! (`crate::registry`): using it after it was released (a `FetchHeaders` copy that outlives its
//! `FetchResponse`, or a forged key) is a clear runtime error, never a read of freed memory.

use super::owned_str;
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::result::{code, invalid_utf8, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A completed response (`VeltFetchResp` in the ABI docs).
pub struct FetchResp {
    status: u16,
    headers: HeaderMap,
    body: Bytes,
}

/// A fetch response handle (a registry key).
pub type FetchRespHandle = Key<FetchResp>;

static RESPONSES: Registry<FetchResp> = Registry::new();

/// The response behind `r`, or a fatal error when it was already released.
fn obj(r: FetchRespHandle) -> Arc<FetchResp> {
    RESPONSES.get(r).unwrap_or_else(|| {
        crate::panic::fatal(concat!(
            "a fetch response (or its headers) was used after it was released: keep the ",
            "FetchResponse alive, or copy what you need first"
        ))
    })
}

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// The pooled client trusting the built-in roots plus `extra_ca_pem` (one per CA text).
fn client(extra_ca_pem: &[u8]) -> Result<HttpClient, VeltErr> {
    type Cache = Mutex<HashMap<Vec<u8>, HttpClient>>;
    static CLIENTS: OnceLock<Cache> = OnceLock::new();
    let clients = CLIENTS.get_or_init(Cache::default);
    let mut clients = clients.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = clients.get(extra_ca_pem) {
        return Ok(c.clone());
    }
    let config = crate::tls::client_config(extra_ca_pem).map_err(|e| invalid(&e))?;
    let https = HttpsConnectorBuilder::new()
        .with_tls_config((*config).clone())
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();
    let c = Client::builder(TokioExecutor::new()).build(https);
    clients.insert(extra_ca_pem.to_vec(), c.clone());
    Ok(c)
}

fn invalid(what: &str) -> VeltErr {
    VeltErr::new(code::INVALID_INPUT, what)
}

unsafe fn build_request(
    method: *const VeltStr,
    url: *const VeltStr,
    headers: *const VeltStr,
    n_headers: u64,
    body: *const VeltStr,
) -> Result<Request<Full<Bytes>>, VeltErr> {
    let uri: Uri = (*url)
        .as_bytes()
        .try_into()
        .map_err(|_| invalid("invalid URL"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return Err(VeltErr::new(
            code::UNSUPPORTED,
            "fetch supports http:// and https:// URLs",
        ));
    }
    let method =
        Method::from_bytes((*method).as_bytes()).map_err(|_| invalid("invalid HTTP method"))?;
    let data = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::copy_from_slice((*body).as_bytes())
    };
    let mut req = Request::new(Full::new(data));
    *req.method_mut() = method;
    *req.uri_mut() = uri;
    for i in 0..n_headers as usize {
        let (n, v) = (&*headers.add(2 * i), &*headers.add(2 * i + 1));
        let name =
            HeaderName::from_bytes(n.as_bytes()).map_err(|_| invalid("invalid header name"))?;
        let value =
            HeaderValue::from_bytes(v.as_bytes()).map_err(|_| invalid("invalid header value"))?;
        req.headers_mut().append(name, value);
    }
    Ok(req)
}

async fn send(client: HttpClient, req: Request<Full<Bytes>>) -> Result<FetchResp, VeltErr> {
    let other = |e: &dyn std::fmt::Display| VeltErr::new(code::OTHER, &format!("{e:#}"));
    let resp = client.request(req).await.map_err(|e| {
        use std::error::Error;
        match e.source().and_then(|s| s.downcast_ref::<std::io::Error>()) {
            Some(io) => VeltErr::from_io(io),
            None => other(&e),
        }
    })?;
    let (parts, body) = resp.into_parts();
    let body = body.collect().await.map_err(|e| other(&e))?.to_bytes();
    Ok(FetchResp {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    })
}

unsafe fn fetch(
    method: *const VeltStr,
    url: *const VeltStr,
    headers: *const VeltStr,
    n_headers: u64,
    body: *const VeltStr,
    ca: &[u8],
) -> *mut VeltFut {
    let req = build_request(method, url, headers, n_headers, body);
    let client = client(ca);
    new_leaf(async move {
        match async { send(client?, req?).await }.await {
            Ok(r) => IoResult::ok(RESPONSES.insert(r)),
            Err(e) => IoResult::err(e),
        }
    })
}

/// `fetch(url, { method, headers, body })` → result slot `IoResult<VeltFetchResp*>`.
/// `headers` points to `2 * n_headers` strings (name, value, name, value...); `body` may be null.
/// All arguments are copied.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch(
    method: *const VeltStr,
    url: *const VeltStr,
    headers: *const VeltStr,
    n_headers: u64,
    body: *const VeltStr,
) -> *mut VeltFut {
    fetch(method, url, headers, n_headers, body, &[])
}

/// `fetch(url, { ca })`: like `velt_rt_http_fetch`, also trusting the PEM CA certificates in
/// `ca` for `https://` (private or test CAs); an invalid PEM fails with `EINVAL`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_ca(
    method: *const VeltStr,
    url: *const VeltStr,
    headers: *const VeltStr,
    n_headers: u64,
    body: *const VeltStr,
    ca: *const VeltStr,
) -> *mut VeltFut {
    fetch(method, url, headers, n_headers, body, (*ca).as_bytes())
}

/// Response status code.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_status(r: FetchRespHandle) -> u32 {
    obj(r).status as u32
}

/// Response header by case-insensitive name: returns 1 and writes `out`, or 0 if absent/not text.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_header(
    r: FetchRespHandle,
    name: *const VeltStr,
    out: *mut VeltStr,
) -> u8 {
    let name = String::from_utf8_lossy((*name).as_bytes());
    match obj(r)
        .headers
        .get(name.as_ref())
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => {
            out.write(owned_str(v));
            1
        }
        None => 0,
    }
}

/// Body as UTF-8 text (copied). `EILSEQ` in `out` if not UTF-8.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_text(
    r: FetchRespHandle,
    out: *mut IoResult<VeltStr>,
) {
    let res = match std::str::from_utf8(&obj(r).body) {
        Ok(s) => IoResult::ok(owned_str(s)),
        Err(_) => IoResult::err(VeltErr::from_io(&invalid_utf8("response body"))),
    };
    res.write_to(out);
}

/// Body as bytes (copied).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_bytes(r: FetchRespHandle, out: *mut VeltBytes) {
    out.write(VeltBytes::from_vec(obj(r).body.to_vec()));
}

/// Free a fetch response (a released key is ignored).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_http_fetch_resp_drop(r: FetchRespHandle) {
    drop(RESPONSES.remove(r));
}
