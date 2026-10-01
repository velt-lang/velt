//! Client side: `connectWebSocket("ws://…" | "wss://…")` over TCP or TLS (rustls, §14.8).

use super::{Transport, Ws, WsObj};
use crate::net::tcp::text_arg;
use crate::result::{code, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use hyper::Uri;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

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

async fn open(url: String, ca: Vec<u8>) -> Result<Ws, VeltErr> {
    let (secure, host, port) = target(&url)?;
    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| VeltErr::from_io(&e))?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Transport> = if secure {
        let config = crate::tls::client_config(&ca).map_err(|e| invalid(&e))?;
        let name = ServerName::try_from(host.clone()).map_err(|_| invalid("invalid host name"))?;
        let tls = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|e| VeltErr::from_io(&e))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    let (ws, _response) = tokio_tungstenite::client_async(url.as_str(), io)
        .await
        .map_err(|e| VeltErr::new(code::OTHER, &format!("WebSocket handshake failed: {e}")))?;
    Ok(ws)
}

/// `connectWebSocket(url, { ca })` → `IoResult<VeltWs*>`. `ca` = extra trusted PEM CA
/// certificates for `wss://` (empty = built-in roots only).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_connect(
    url: *const VeltStr,
    ca: *const VeltStr,
) -> *mut VeltFut {
    let url = text_arg(url);
    let ca = (*ca).as_bytes().to_vec();
    new_leaf(async move {
        match open(url, ca).await {
            Ok(ws) => IoResult::ok(super::SOCKETS.insert(WsObj::open(ws))),
            Err(e) => IoResult::err(e),
        }
    })
}
