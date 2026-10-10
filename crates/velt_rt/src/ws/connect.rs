//! Client side: `connectWebSocket("ws://…" | "wss://…")` over TCP or TLS (rustls, §14.8).

use super::{Conn, Role, Transport, WsObj};
use crate::net::tcp::text_arg;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use hyper::Uri;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

fn invalid(message: &str) -> VeltErr {
    VeltErr::new(code::INVALID_INPUT, message)
}

/// Target of a `ws://`/`wss://` URL: (secure, host, port).
fn target(url: &str) -> Result<(bool, String, u16), VeltErr> {
    let uri: Uri = url.parse().map_err(|_| invalid("invalid WebSocket URL"))?;
    let secure = match uri.scheme_str() {
        Some("ws") => false,
        Some("wss") => true,
        _ => return Err(invalid("WebSocket URLs start with ws:// or wss://")),
    };
    let host = uri
        .host()
        .ok_or_else(|| invalid("WebSocket URL without a host"))?;
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    Ok((secure, host, port))
}

fn handshake_failed(what: &str) -> VeltErr {
    VeltErr::new(code::OTHER, &format!("WebSocket handshake failed: {what}"))
}

/// Sends the HTTP/1.1 upgrade request and checks the `101` response; returns the bytes read past
/// the response (the first frames, if the server was quick).
async fn handshake<S: Transport + ?Sized>(
    io: &mut S,
    uri: &Uri,
    host: &str,
    port: u16,
    secure: bool,
) -> Result<Vec<u8>, VeltErr> {
    let key = generate_key();
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    let default_port = if secure { 443 } else { 80 };
    let host_header = match (host.contains(':'), port == default_port) {
        (true, true) => format!("[{host}]"),
        (true, false) => format!("[{host}]:{port}"),
        (false, true) => host.to_string(),
        (false, false) => format!("{host}:{port}"),
    };
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n"
    );
    io.write_all(request.as_bytes())
        .await
        .map_err(|e| VeltErr::from_io(&e))?;
    io.flush().await.map_err(|e| VeltErr::from_io(&e))?;
    let mut buf = Vec::with_capacity(1024);
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 16 * 1024 {
            return Err(handshake_failed("response head too large"));
        }
        let n = io
            .read_buf(&mut buf)
            .await
            .map_err(|e| VeltErr::from_io(&e))?;
        if n == 0 {
            return Err(handshake_failed("connection closed during the handshake"));
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code != "101" {
        return Err(handshake_failed(&format!(
            "HTTP error: {}",
            status.split_once(' ').map_or(status, |(_, rest)| rest)
        )));
    }
    let accept = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("sec-websocket-accept"))
        .map(|(_, v)| v.trim().to_string());
    if accept.as_deref() != Some(derive_accept_key(key.as_bytes()).as_str()) {
        return Err(handshake_failed("wrong Sec-WebSocket-Accept"));
    }
    Ok(buf[end + 4..].to_vec())
}

async fn open(url: String, ca: Vec<u8>) -> Result<Conn, VeltErr> {
    let (secure, host, port) = target(&url)?;
    let uri: Uri = url.parse().map_err(|_| invalid("invalid WebSocket URL"))?;
    let mut tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| VeltErr::from_io(&e))?;
    let _ = tcp.set_nodelay(true);
    if !secure {
        let rest = handshake(&mut tcp, &uri, &host, port, false).await?;
        return Ok(Conn::start(Role::Client, Some(tcp), None, rest));
    }
    let config = crate::tls::client_config(&ca).map_err(|e| invalid(&e))?;
    let name = ServerName::try_from(host.clone()).map_err(|_| invalid("invalid host name"))?;
    let mut tls: Box<dyn Transport> = Box::new(
        TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|e| VeltErr::from_io(&e))?,
    );
    let rest = handshake(&mut *tls, &uri, &host, port, true).await?;
    Ok(Conn::start(Role::Client, None, Some(tls), rest))
}

/// `connectWebSocket(url, { ca })` → `IoResult<VeltWs*>`. `ca` = extra trusted PEM CA
/// certificates for `wss://` (empty = built-in roots only).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_connect(
    url: *const VeltStr,
    ca: *const VeltStr,
) -> *mut VeltFut {
    let url = text_arg(url);
    let ca = (*ca).text_lossy().as_bytes().to_vec();
    new_leaf(async move {
        match open(url, ca).await {
            Ok(conn) => IoResult::ok(super::SOCKETS.insert(WsObj::open(conn))),
            Err(e) => IoResult::err(e),
        }
    })
}
