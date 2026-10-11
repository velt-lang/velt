//! Sending a `fetch` request: the pooled clients (`client`), the outgoing request
//! ([`Outgoing`]), redirects as the Fetch standard follows them ([`send`]), and the error
//! messages Node's `fetch` gives.

use super::stamp::{opened_before, Stamped};
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
pub(super) type HttpClient = Client<Stamped<HttpsConnector<HttpConnector>>, Full<Bytes>>;

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
    Ok(Client::builder(TokioExecutor::new()).build(Stamped(https)))
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
/// and `accept-encoding` (`codings`), then `host` when it is given (else hyper adds it). The map
/// has room for all four, so none grows it.
pub(super) fn with_defaults(
    own: HeaderMap,
    codings: &'static str,
    host: Option<HeaderValue>,
) -> HeaderMap {
    let defaults = [
        (header::ACCEPT, HeaderValue::from_static("*/*")),
        (header::USER_AGENT, HeaderValue::from_static("velt")),
        (header::ACCEPT_ENCODING, HeaderValue::from_static(codings)),
    ];
    if own.is_empty() {
        // Most requests: a copy of the defaults, made once, which is cheaper than hashing their
        // names each time.
        static PLAIN: [OnceLock<HeaderMap>; 2] = [OnceLock::new(), OnceLock::new()];
        let https = codings == super::decode::accept_encoding(true);
        let mut map = PLAIN[usize::from(https)]
            .get_or_init(|| {
                let mut map = HeaderMap::with_capacity(ADDED);
                for (name, value) in defaults {
                    map.insert(name, value);
                }
                map
            })
            .clone();
        if let Some(h) = host {
            map.insert(header::HOST, h);
        }
        return map;
    }
    let added = defaults.into_iter().chain(host.map(|h| (header::HOST, h)));
    let mut map = own;
    map.reserve(ADDED);
    for (name, value) in added {
        if !map.contains_key(&name) {
            map.insert(name, value);
        }
    }
    map
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The request to send for `o`. Its headers are moved into it unless a redirect may need them
/// again (`keep`), so a request that is not followed copies none.
fn request(o: &mut Outgoing, keep: bool) -> Request<Full<Bytes>> {
    let mut req = Request::new(Full::new(o.body.clone()));
    *req.method_mut() = o.method.clone();
    let host = o.url.host();
    // A redirect makes a new URI from `o.url.href`: the request can have this one.
    *req.uri_mut() = std::mem::take(&mut o.url.uri);
    let own = if keep {
        o.headers.clone()
    } else {
        std::mem::take(&mut o.headers)
    };
    *req.headers_mut() = with_defaults(own, o.codings, host);
    req
}

/// Methods that may be sent twice (RFC 9110 §9.2.2).
fn idempotent(m: &Method) -> bool {
    matches!(
        *m,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE | Method::PUT | Method::DELETE
    )
}

/// Whether `e` is a connection that broke before a response (not a failed connect, which a new
/// connection would not fix); [`opened_before`] tells whether it was a reused one.
fn stale_connection(e: &hyper_util::client::legacy::Error) -> bool {
    use std::error::Error;
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    if e.is_connect() {
        return false;
    }
    let mut source = e.source();
    while let Some(s) = source {
        if let Some(h) = s.downcast_ref::<hyper::Error>() {
            if h.is_incomplete_message() || h.is_canceled() || h.is_closed() {
                return true;
            }
        }
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof
            );
        }
        source = s.source();
    }
    false
}

/// A response with the URL it came from and whether a redirect led there.
pub(super) struct Received {
    pub response: Response<Incoming>,
    /// The WHATWG serialization.
    pub url: Bytes,
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
        // What the request is built from, kept for a second send: a `Uri` clone shares its
        // bytes and the caller's header map is empty unless they set headers, so keeping them
        // allocates nothing for most requests (the request itself is only rebuilt on a retry).
        let again = idempotent(&o.method).then(|| (o.url.uri.clone(), o.headers.clone()));
        let req = request(&mut o, mode == Redirect::Follow);
        let sent = std::time::Instant::now();
        let response = match client.request(req).await {
            Ok(r) => r,
            // A reused pooled keep-alive connection the server closed meanwhile fails before a
            // response: an idempotent request goes out once more, on another connection
            // (#1020). Never on a fresh connection: that server got the request and failed.
            Err(e) => match again {
                Some((uri, headers)) if stale_connection(&e) && opened_before(&e, sent) => {
                    (o.url.uri, o.headers) = (uri, headers);
                    let req = request(&mut o, mode == Redirect::Follow);
                    client.request(req).await.map_err(|e| failed(&e))?
                }
                _ => return Err(failed(&e)),
            },
        };
        let status = response.status();
        // Only a redirect's `location` is looked up.
        let location = (status.is_redirection() && mode != Redirect::Manual)
            .then(|| response.headers().get(header::LOCATION))
            .flatten();
        if location.is_none() {
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
