//! Sending a `fetch` request: the pooled clients (`client`), the outgoing request
//! ([`Outgoing`]), redirects as the Fetch standard follows them ([`send`]), and the error
//! messages Node's `fetch` gives.

use super::target::Target;
use super::Redirect;
use crate::result::{code, VeltErr};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use url::Url;

/// A pooled HTTP/1.1 + HTTP/2 client (keep-alive connections are reused per origin).
pub(super) type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

/// How long connecting may take before `fetch` fails with `ETIMEDOUT` (undici's default).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The most redirects one `fetch` follows (the Fetch standard's limit).
const MAX_REDIRECTS: usize = 20;

fn build(extra_ca_pem: &[u8]) -> Result<HttpClient, VeltErr> {
    let config = crate::tls::client_config(extra_ca_pem).map_err(|e| invalid(&e))?;
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_nodelay(true);
    http.set_connect_timeout(Some(CONNECT_TIMEOUT));
    let https = HttpsConnectorBuilder::new()
        .with_tls_config((*config).clone())
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

/// The pooled client trusting the built-in roots plus `extra_ca_pem` (one per CA text; the
/// common case, no extra CA, skips the lock).
pub(super) fn client(extra_ca_pem: &[u8]) -> Result<HttpClient, VeltErr> {
    if extra_ca_pem.is_empty() {
        static DEFAULT: OnceLock<HttpClient> = OnceLock::new();
        if let Some(c) = DEFAULT.get() {
            return Ok(c.clone());
        }
        let c = build(&[])?;
        return Ok(DEFAULT.get_or_init(|| c).clone());
    }
    type Cache = Mutex<HashMap<Vec<u8>, HttpClient>>;
    static CLIENTS: OnceLock<Cache> = OnceLock::new();
    let clients = CLIENTS.get_or_init(Cache::default);
    let mut clients = clients.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = clients.get(extra_ca_pem) {
        return Ok(c.clone());
    }
    let c = build(extra_ca_pem)?;
    clients.insert(extra_ca_pem.to_vec(), c.clone());
    Ok(c)
}

pub(super) fn invalid(what: &str) -> VeltErr {
    VeltErr::new(code::INVALID_INPUT, what)
}

/// A request as `fetch` sends it (again, after a redirect).
pub(super) struct Outgoing {
    pub method: Method,
    pub url: Target,
    /// The caller's headers: each request adds the defaults to them ([`with_defaults`]), so a
    /// request without headers of its own has nothing to copy for a redirect.
    pub headers: HeaderMap,
    /// The `accept-encoding` sent unless the caller set one: the codings the first URL's scheme
    /// can decode.
    pub codings: &'static str,
    pub body: Bytes,
}

/// The caller's header list `[name, value, …]` as a header map (empty, which allocates
/// nothing, when there are none).
pub(super) fn header_map(flat: &[&[u8]]) -> Result<HeaderMap, VeltErr> {
    if flat.is_empty() {
        return Ok(HeaderMap::new());
    }
    let mut map = HeaderMap::with_capacity(flat.len() / 2 + ADDED);
    for [name, value] in flat.as_chunks::<2>().0 {
        let name = HeaderName::from_bytes(name)
            .map_err(|_| invalid(&format!("invalid header name {:?}", lossy(name))))?;
        let value = HeaderValue::from_bytes(value)
            .map_err(|_| invalid(&format!("invalid value of header {name}")))?;
        map.append(name, value);
    }
    Ok(map)
}

/// The headers added to the caller's: the three defaults and the `host` hyper adds.
const ADDED: usize = 4;

/// `own` (the caller's headers) with what Node adds when it is missing: `accept`, `user-agent`
/// and `accept-encoding` (`codings`). The map has room for them and for the `host` hyper adds,
/// so neither grows it.
pub(super) fn with_defaults(own: HeaderMap, codings: &'static str) -> HeaderMap {
    let defaults = [
        (header::ACCEPT, "*/*"),
        (header::USER_AGENT, "velt"),
        (header::ACCEPT_ENCODING, codings),
    ];
    if own.is_empty() {
        // Most requests: nothing to look up.
        let mut map = HeaderMap::with_capacity(ADDED);
        for (name, value) in defaults {
            map.insert(name, HeaderValue::from_static(value));
        }
        return map;
    }
    let mut map = own;
    map.reserve(ADDED);
    for (name, value) in defaults {
        if !map.contains_key(&name) {
            map.insert(name, HeaderValue::from_static(value));
        }
    }
    map
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The request to send for `o`. Its URI and headers are moved into it unless a redirect may need
/// them again (`keep`), so a request that is not followed copies neither.
fn request(o: &mut Outgoing, keep: bool) -> Request<Full<Bytes>> {
    let mut req = Request::new(Full::new(o.body.clone()));
    *req.method_mut() = o.method.clone();
    let own = if keep {
        *req.uri_mut() = o.url.uri.clone();
        o.headers.clone()
    } else {
        *req.uri_mut() = std::mem::take(&mut o.url.uri);
        std::mem::take(&mut o.headers)
    };
    *req.headers_mut() = with_defaults(own, o.codings);
    req
}

/// A response with the URL it came from and whether a redirect led there.
pub(super) struct Received {
    pub response: Response<Incoming>,
    /// The WHATWG serialization.
    pub url: String,
    pub redirected: bool,
}

/// Send `o`, following redirects as `mode` says.
pub(super) async fn send(
    client: &HttpClient,
    mut o: Outgoing,
    mode: Redirect,
) -> Result<Received, VeltErr> {
    let mut redirected = false;
    for _ in 0..=MAX_REDIRECTS {
        let req = request(&mut o, mode == Redirect::Follow);
        let response = client.request(req).await.map_err(|e| failed(&e))?;
        let status = response.status();
        let location = response.headers().get(header::LOCATION);
        if !status.is_redirection() || mode == Redirect::Manual || location.is_none() {
            return Ok(Received {
                response,
                url: o.url.href,
                redirected,
            });
        }
        if mode == Redirect::Error {
            return Err(VeltErr::new(
                code::OTHER,
                "fetch failed: unexpected redirect",
            ));
        }
        let base = o.url.url()?;
        let next = location
            .and_then(|l| l.to_str().ok())
            .and_then(|l| base.join(l).ok())
            .ok_or_else(|| VeltErr::new(code::OTHER, "fetch failed: invalid redirect URL"))?;
        follow(&mut o, &base, status, next)?;
        redirected = true;
    }
    Err(VeltErr::new(
        code::OTHER,
        "fetch failed: redirect count exceeded",
    ))
}

/// Turn `o` into the request a redirect with `status` to `next` asks for (Fetch standard,
/// "HTTP-redirect fetch"); `base` is `o`'s URL, parsed.
fn follow(o: &mut Outgoing, base: &Url, status: StatusCode, mut next: Url) -> Result<(), VeltErr> {
    if !matches!(next.scheme(), "http" | "https") {
        return Err(VeltErr::new(
            code::OTHER,
            "fetch failed: redirect to a URL that is not http: or https:",
        ));
    }
    next.set_fragment(None);
    let to_get = (status == StatusCode::SEE_OTHER && o.method != Method::HEAD)
        || (matches!(status.as_u16(), 301 | 302) && o.method == Method::POST);
    if to_get {
        o.method = Method::GET;
        o.body = Bytes::new();
        for h in [
            header::CONTENT_TYPE,
            header::CONTENT_LENGTH,
            header::CONTENT_ENCODING,
            header::CONTENT_LANGUAGE,
            header::CONTENT_LOCATION,
        ] {
            o.headers.remove(h);
        }
    }
    if next.origin() != base.origin() {
        for h in [
            header::AUTHORIZATION,
            header::COOKIE,
            header::PROXY_AUTHORIZATION,
            header::HOST,
        ] {
            o.headers.remove(h);
        }
    }
    o.url = Target::from_url(next)?;
    Ok(())
}

/// The error of a request that failed, `fetch failed: <cause>` (Node's message), with the code
/// of the I/O error that caused it (`ECONNREFUSED`, `ETIMEDOUT`, …) when there is one.
fn failed(e: &(dyn std::error::Error + 'static)) -> VeltErr {
    let mut source = Some(e);
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            let code = crate::result::code_of(io);
            return VeltErr::new(code, &format!("fetch failed: {io}"));
        }
        source = s.source();
    }
    let mut message = format!("fetch failed: {e}");
    let mut cause = e.source();
    while let Some(c) = cause {
        message.push_str(&format!(": {c}"));
        cause = c.source();
    }
    VeltErr::new(code::OTHER, &message)
}

/// [`failed`] for an error while the body was received.
pub(super) fn body_failed(e: &hyper::Error) -> VeltErr {
    failed(e)
}
