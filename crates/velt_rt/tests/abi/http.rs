//! HTTP: a compiled request handler served by `velt_rt_http_serve`, exercised with raw HTTP/1.1
//! keep-alive requests from plain threads and with the runtime's own `fetch`.

use super::fake::{arg, block_on_fut, fut_result, ok, take_string};
use crate::bytes::VeltBytes;
use crate::http::client::*;
use crate::http::request::*;
use crate::http::respond::*;
use crate::http::response::*;
use crate::http::server::*;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::str_array::{velt_rt_str_array_drop, VeltStrArray};
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, VeltFut, PENDING, READY};
use crate::timer::velt_rt_sleep;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::mem::MaybeUninit;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

// async (req) => { await sleep(1); hits.add(1); const body = await req.text();
//   const url = new URL(req.url);
//   const res = url.pathname == "/json" ? Response.json({ ok: true })
//        : new Response(`${req.method} ${url.pathname}?${query} [${req.headers.get("x-test") ?? "-"}] ${body}`,
//                       { status: url.pathname == "/missing" ? 404 : 200 });
//   res.headers.set("x-path", url.pathname); return res; }
/// The handler closure's environment: the drop-function word every closure environment starts
/// with, then the captured counter.
#[repr(C)]
struct Env {
    drop: Option<unsafe extern "C" fn(*mut c_void)>,
    hits: &'static AtomicU64,
}

#[repr(C)]
struct Handler {
    result: RespHandle,
    tag: u32,
    req: ReqHandle,
    env: *const AtomicU64,
    fut: *mut VeltFut,
}

unsafe extern "C" fn handler_init(env: *mut c_void, req: *mut ReqObj, state: *mut u8) {
    let h = Handler {
        result: RespHandle::NULL,
        tag: 0,
        req: ReqHandle::from_bits(req as usize as u64),
        env: (*(env as *const Env)).hits,
        fut: null_mut(),
    };
    (state as *mut Handler).write(h);
}

unsafe fn text_of(f: unsafe extern "C" fn(ReqHandle, *mut VeltStr), req: ReqHandle) -> String {
    let mut out = MaybeUninit::uninit();
    f(req, out.as_mut_ptr());
    take_string(out.assume_init())
}

/// The path and query of a request's absolute URL (`http://x/echo?a=1` → `/echo`, `a=1`).
unsafe fn path_and_query(req: ReqHandle) -> (String, String) {
    let url = text_of(velt_rt_http_req_url, req);
    let after_scheme = url.strip_prefix("http://").expect("an absolute http: URL");
    let rest = &after_scheme[after_scheme.find('/').expect("a path")..];
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    (path.to_string(), query.to_string())
}

/// A response as std builds one: `headers` are `[name, value, …]`, the text body is taken.
unsafe fn build(status: u32, headers: &[&str], body: String, implied: u32) -> RespHandle {
    let list = VeltStrArray::from_vec(headers.iter().map(|h| VeltStr::from_text(h)).collect());
    let mut owned = VeltStr::from_vec(body.into_bytes());
    let bytes = VeltBytes::from_vec(vec![]);
    let r = velt_rt_http_resp_build(
        status,
        &VeltStr::empty(),
        &list,
        1,
        &mut owned,
        &bytes,
        implied,
    );
    assert!(owned.is_empty(), "body ownership moved to the response");
    r
}

/// Responses handed straight to their request (`velt_rt_http_req_respond`) and left in the
/// handler's frame.
static IN_FRAME: AtomicU64 = AtomicU64::new(0);

/// A text response handed to request `req`, as std hands over a complete body: what the
/// handler returns. Its first header goes as its own pair, as std passes a header set on a new
/// response: as written, `X-Path: ` and the value between spaces (the runtime lowercases and
/// trims); `extra` headers (`[name, value, …]`, normalized) follow in a list.
unsafe fn hand_over(
    req: ReqHandle,
    status: u32,
    path: &str,
    extra: &[&str],
    body: String,
) -> RespHandle {
    let (name, value) = (
        VeltStr::from_text("X-Path"),
        VeltStr::from_text(&format!(" {path}\t")),
    );
    let mut owned = VeltStr::from_vec(body.into_bytes());
    let bytes = VeltBytes::from_vec(vec![]);
    let reason = VeltStr::empty();
    let r = if extra.is_empty() {
        velt_rt_http_req_respond(req.bits(), status, &reason, &name, &value, 1, &mut owned, 1)
    } else {
        let list = VeltStrArray::from_vec(extra.iter().map(|h| VeltStr::from_text(h)).collect());
        velt_rt_http_req_respond_list(
            req.bits(),
            status,
            &reason,
            &name,
            &value,
            &list,
            1,
            &mut owned,
            &bytes,
            1,
        )
    };
    assert!(owned.is_empty(), "body ownership moved to the response");
    match r {
        crate::http::context::RESPONDED => IN_FRAME.fetch_add(1, Ordering::SeqCst),
        0 => panic!("a valid response was refused"),
        _ => 0,
    };
    RespHandle::from_bits(r)
}

unsafe fn respond(req: ReqHandle, body: String) -> RespHandle {
    let method = text_of(velt_rt_http_req_method, req);
    let (path, query) = path_and_query(req);
    let mut h = MaybeUninit::uninit();
    let hdr = if velt_rt_http_req_header(req, &arg("X-Test"), h.as_mut_ptr()) == 1 {
        take_string(h.assume_init())
    } else {
        "-".into()
    };
    if path == "/json" {
        let json = r#"{"ok":true}"#.to_string();
        return build(200, &["x-path", &path], json, 2);
    }
    let status = if path == "/missing" { 404 } else { 200 };
    let text = format!("{method} {path}?{query} [{hdr}] {body}");
    let extra: &[&str] = if path == "/missing" {
        &["x-extra", "1", "x-extra", "2"]
    } else {
        &[]
    };
    hand_over(req, status, &path, extra, text)
}

/// Handlers of `/slow` started (`a_handler_finishes_after_its_client_left`).
static SLOW_STARTED: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn handler_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Handler);
    loop {
        match st.tag {
            // `/fast` skips the sleep (throughput test); `/slow` gives a client time to leave.
            0 => match path_and_query(st.req).0.as_str() {
                "/fast" => st.tag = 1,
                "/slow" => {
                    SLOW_STARTED.fetch_add(1, Ordering::SeqCst);
                    (st.fut, st.tag) = (velt_rt_sleep(200), 1);
                }
                _ => (st.fut, st.tag) = (velt_rt_sleep(1), 1),
            },
            1 => {
                if !st.fut.is_null() {
                    if velt_rt_fut_poll(st.fut, cx) == PENDING {
                        return PENDING;
                    }
                    velt_rt_fut_drop(st.fut);
                }
                (*st.env).fetch_add(1, Ordering::SeqCst);
                (st.fut, st.tag) = (velt_rt_http_req_text(st.req), 2);
            }
            2 => {
                if velt_rt_fut_poll(st.fut, cx) == PENDING {
                    return PENDING;
                }
                let body = take_string(ok(fut_result::<IoResult<VeltStr>>(st.fut)));
                velt_rt_fut_drop(st.fut);
                st.fut = null_mut();
                st.result = respond(st.req, body);
                velt_rt_http_req_drop(st.req);
                st.tag = 3;
                return READY;
            }
            _ => unreachable!(),
        }
    }
}

unsafe extern "C" fn handler_drop(s: *mut u8) {
    let st = &mut *(s as *mut Handler);
    if !st.fut.is_null() {
        velt_rt_fut_drop(st.fut);
    }
    velt_rt_http_req_drop(st.req);
}

fn start_server(
    hits: &'static AtomicU64,
    drop: Option<unsafe extern "C" fn(*mut c_void)>,
) -> ServerHandle {
    let env = Box::into_raw(Box::new(Env { drop, hits }));
    let handler = VeltHandler {
        init: handler_init,
        poll: handler_poll,
        drop: handler_drop,
        state_size: size_of::<Handler>() as u64,
        state_align: 8,
        env: env as *mut c_void,
    };
    ok(block_on_fut::<IoResult<ServerHandle>>(unsafe {
        velt_rt_http_serve(&arg("127.0.0.1:0"), &handler)
    }))
}

static HITS: AtomicU64 = AtomicU64::new(0);

/// One server shared by the tests below (never closed); returns its port.
fn server_port() -> u32 {
    static PORT: OnceLock<u32> = OnceLock::new();
    *PORT.get_or_init(|| unsafe { velt_rt_http_server_port(start_server(&HITS, None)) })
}

/// Minimal blocking HTTP/1.1 client: (status, headers lowercase, body).
fn read_response(r: &mut BufReader<std::net::TcpStream>) -> (u32, Vec<(String, String)>, String) {
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = Vec::new();
    loop {
        line.clear();
        r.read_line(&mut line).unwrap();
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (n, v) = l.split_once(':').unwrap();
        headers.push((n.to_ascii_lowercase(), v.trim().to_string()));
    }
    let len: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .unwrap()
        .1
        .parse()
        .unwrap();
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    (status, headers, String::from_utf8(body).unwrap())
}

fn header<'a>(h: &'a [(String, String)], name: &str) -> &'a str {
    &h.iter().find(|(n, _)| n == name).unwrap().1
}

#[test]
fn raw_http11_keep_alive() {
    let port = server_port();
    let s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    w.write_all(
        b"POST /echo?a=1&b=2 HTTP/1.1\r\nHost: x\r\nX-Test: yes\r\nContent-Length: 5\r\n\r\nhello",
    )
    .unwrap();
    let (status, h, body) = read_response(&mut r);
    assert_eq!(
        (status, body.as_str()),
        (200, "POST /echo?a=1&b=2 [yes] hello")
    );
    assert_eq!(header(&h, "content-type"), "text/plain;charset=UTF-8");
    assert_eq!(header(&h, "x-path"), "/echo");
    // Same connection (keep-alive).
    w.write_all(b"GET /json HTTP/1.1\r\nHost: x\r\n\r\nGET /missing HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let (status, h, body) = read_response(&mut r);
    assert_eq!(
        (status, body.as_str(), header(&h, "content-type")),
        (200, r#"{"ok":true}"#, "application/json")
    );
    let (status, h, body) = read_response(&mut r);
    assert_eq!((status, body.as_str()), (404, "GET /missing? [-] "));
    // The first header as its own pair, the rest in a list, in order.
    let names: Vec<_> = h.iter().map(|(n, v)| format!("{n}={v}")).collect();
    let x: Vec<_> = names.iter().filter(|n| n.starts_with("x-")).collect();
    assert_eq!(x, ["x-path=/missing", "x-extra=1", "x-extra=2"]);
    // The text responses went straight to their request's frame.
    assert!(IN_FRAME.load(Ordering::SeqCst) >= 2);
}

#[test]
fn concurrent_clients() {
    let port = server_port();
    let before = HITS.load(Ordering::SeqCst);
    let threads: Vec<_> = (0..16)
        .map(|t| {
            std::thread::spawn(move || {
                let s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                for i in 0..25 {
                    write!(w, "GET /t{t}?i={i} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                    let (status, _, body) = read_response(&mut r);
                    assert_eq!((status, body), (200, format!("GET /t{t}?i={i} [-] ")));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(HITS.load(Ordering::SeqCst) >= before + 16 * 25);
}

/// Throughput sanity check: 32 keep-alive clients, 2000 sequential requests each, immediate
/// handler, every response checked. Prints requests/s (use `--release --nocapture` for real
/// numbers) but sets no time limit, which a loaded machine could miss; `http_bench` and
/// `bench/http/` measure throughput.
#[test]
fn keep_alive_throughput() {
    let port = server_port();
    let (clients, per_client) = (32, 2000);
    let t = std::time::Instant::now();
    let threads: Vec<_> = (0..clients)
        .map(|_| {
            std::thread::spawn(move || {
                let s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
                s.set_nodelay(true).unwrap();
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                for _ in 0..per_client {
                    w.write_all(b"GET /fast HTTP/1.1\r\nHost: x\r\n\r\n")
                        .unwrap();
                    assert_eq!(read_response(&mut r).0, 200);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let n = clients * per_client;
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "http keep-alive: {n} requests in {secs:.2}s = {:.0} req/s",
        n as f64 / secs
    );
}

/// `fetch` through the C ABI: `headers` borrows `strs` (`cap` 0: nothing to free).
unsafe fn send(method: &str, url: &str, strs: &[VeltStr], body: Option<&str>) -> *mut VeltFut {
    let headers = VeltStrArray {
        ptr: strs.as_ptr() as *mut VeltStr,
        len: strs.len() as u64,
        cap: 0,
    };
    let mut text = arg(body.unwrap_or(""));
    let mut bytes = VeltBytes::from_vec(vec![]);
    let kind = if body.is_some() { 1 } else { 0 };
    velt_rt_http_fetch_send(
        &arg(method),
        &arg(url),
        &headers,
        kind,
        &mut text,
        &mut bytes,
        0,
        0,
        &arg(""),
    )
}

unsafe fn text(r: FetchRespHandle) -> String {
    take_string(ok(block_on_fut::<IoResult<VeltStr>>(
        velt_rt_http_fetch_resp_text(r),
    )))
}

#[test]
fn fetch_get_and_post() {
    let port = server_port();
    let url = format!("http://127.0.0.1:{port}/json");
    let r = ok(block_on_fut::<IoResult<FetchRespHandle>>(unsafe {
        send("GET", &url, &[], None)
    }));
    unsafe {
        assert_eq!(velt_rt_http_fetch_resp_status(r), 200);
        assert!(!velt_rt_http_fetch_resp_redirected(r));
        let mut out = MaybeUninit::uninit();
        velt_rt_http_fetch_resp_status_text(r, out.as_mut_ptr());
        assert_eq!(take_string(out.assume_init()), "OK");
        let mut out = MaybeUninit::uninit();
        velt_rt_http_fetch_resp_url(r, out.as_mut_ptr());
        assert_eq!(take_string(out.assume_init()), url);
        let mut list = MaybeUninit::<VeltStrArray>::uninit();
        velt_rt_http_fetch_resp_headers(r, list.as_mut_ptr());
        let mut list = list.assume_init();
        let flat: Vec<String> = (0..list.len as usize)
            .map(|i| String::from_utf8((*list.ptr.add(i)).as_bytes().to_vec()).unwrap())
            .collect();
        velt_rt_str_array_drop(&mut list);
        let ct = flat.iter().position(|n| n == "content-type").unwrap();
        assert_eq!(flat[ct + 1], "application/json");
        assert_eq!(text(r), r#"{"ok":true}"#);
        velt_rt_http_fetch_resp_drop(r);
    }

    let url = format!("http://127.0.0.1:{port}/post?z");
    let headers = [arg("x-test"), arg("fetch")];
    let r = ok(block_on_fut::<IoResult<FetchRespHandle>>(unsafe {
        send("POST", &url, &headers, Some("data!"))
    }));
    unsafe {
        assert_eq!(text(r), "POST /post?z [fetch] data!");
        velt_rt_http_fetch_resp_drop(r);
    }

    // Only http:// and https:// are fetched (checked before any connection is made).
    let ftp = block_on_fut::<IoResult<FetchRespHandle>>(unsafe {
        send("GET", "ftp://example.com/", &[], None)
    });
    assert_eq!(ftp.err.code, crate::result::code::UNSUPPORTED);
    take_string(ftp.err.message);
}

#[test]
fn close_stops_the_server() {
    static CLOSED_HITS: AtomicU64 = AtomicU64::new(0);
    let server = start_server(&CLOSED_HITS, None);
    let port = unsafe { velt_rt_http_server_port(server) } as u16;
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut w = s.try_clone().unwrap();
    w.write_all(b"GET /x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut BufReader::new(s)).0, 200);
    unsafe { velt_rt_http_server_close(server) };
    // A hang guard, not a deadline: on a loaded machine the accept loop may take a while.
    let t = std::time::Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            t.elapsed().as_secs() < 60,
            "server still accepting after close()"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(CLOSED_HITS.load(Ordering::SeqCst), 1);
}

static RELEASED: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn release_env(env: *mut c_void) {
    drop(Box::from_raw(env as *mut Env));
    RELEASED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn shutdown_waits_for_requests_then_releases_the_handler() {
    static SHUTDOWN_HITS: AtomicU64 = AtomicU64::new(0);
    let server = start_server(&SHUTDOWN_HITS, Some(release_env));
    let port = unsafe { velt_rt_http_server_port(server) } as u16;
    // A keep-alive connection that stays open: shutdown must close it, not wait for the client.
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut w = s.try_clone().unwrap();
    w.write_all(b"GET /x HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut BufReader::new(s)).0, 200);
    assert_eq!(RELEASED.load(Ordering::SeqCst), 0);
    block_on_fut::<()>(unsafe { velt_rt_http_server_shutdown(server) });
    assert_eq!(RELEASED.load(Ordering::SeqCst), 1);
    assert_eq!(SHUTDOWN_HITS.load(Ordering::SeqCst), 1);
}

#[test]
fn a_handler_finishes_after_its_client_left() {
    static LEFT_HITS: AtomicU64 = AtomicU64::new(0);
    let server = start_server(&LEFT_HITS, None);
    let port = unsafe { velt_rt_http_server_port(server) } as u16;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    // Hang up once the handler runs (it then sleeps 200 ms): hyper drops the request, the
    // handler still completes. Waiting for a fixed time instead could hang up before the
    // request was read on a loaded machine, and then no handler would ever run.
    let t = std::time::Instant::now();
    while SLOW_STARTED.load(Ordering::SeqCst) == 0 {
        assert!(t.elapsed().as_secs() < 60, "the handler did not start");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drop(s);
    // A hang guard, not a deadline.
    let t = std::time::Instant::now();
    while LEFT_HITS.load(Ordering::SeqCst) == 0 {
        assert!(t.elapsed().as_secs() < 60, "the handler was cancelled");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unsafe { velt_rt_http_server_close(server) };
}

#[test]
fn pipelined_requests_are_all_answered_even_after_a_half_close() {
    let port = server_port() as u16;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(&b"GET /fast HTTP/1.1\r\nHost: x\r\n\r\n".repeat(5))
        .unwrap();
    // The client is done sending (like `printf … | nc`): responses must still arrive.
    s.shutdown(std::net::Shutdown::Write).unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    assert_eq!(all.matches("HTTP/1.1 200").count(), 5, "{all}");
}
