//! Server side: accepting a WebSocket upgrade requested through the HTTP server.

use super::{invalid, Conn, Role, WsHandle, WsObj};
use crate::http::body::RespBody;
use crate::http::response::{RespHandle, RespObj};
use crate::http::upgrade;
use crate::result::IoResult;
use crate::str::VeltStr;
use bytes::Bytes;
use hyper::header::{HeaderValue, CONNECTION, SEC_WEBSOCKET_ACCEPT, UPGRADE};
use hyper::upgrade::OnUpgrade;
use hyper::{Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::io;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

/// `{ VeltWs* ws; VeltResp* response; }`: the connection and the `101` response the handler
/// must return for the upgrade to happen.
#[repr(C)]
pub struct VeltWsAccept {
    /// The connection (usable once the response has been sent).
    pub ws: WsHandle,
    /// `101 Switching Protocols` with the handshake headers.
    pub response: RespHandle,
}

/// Waits for the parked upgrade to complete and starts the connection: over the raw TCP stream
/// when the connection was plain TCP (bytes hyper read past the request come along), else over
/// the upgraded connection (TLS).
pub(super) async fn finish(pending: &std::sync::Mutex<Option<OnUpgrade>>) -> io::Result<Conn> {
    let on = pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| io::Error::other("the WebSocket upgrade was already consumed"))?;
    let upgraded = on.await.map_err(|e| {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("the WebSocket upgrade did not happen (was the 101 response returned?): {e}"),
        )
    })?;
    Ok(
        match hyper_util::server::conn::auto::upgrade::downcast::<TokioIo<TcpStream>>(upgraded) {
            Ok(parts) => Conn::start(
                Role::Server,
                Some(parts.io.into_inner()),
                None,
                parts.read_buf.to_vec(),
            ),
            Err(upgraded) => Conn::start(
                Role::Server,
                None,
                Some(Box::new(TokioIo::new(upgraded))),
                Vec::new(),
            ),
        },
    )
}

fn switching_protocols(sec_key: &[u8]) -> RespObj {
    let mut r = Response::new(RespBody::full(Bytes::new()));
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = r.headers_mut();
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&derive_accept_key(sec_key)) {
        h.insert(SEC_WEBSOCKET_ACCEPT, v);
    }
    r
}

/// `upgradeWebSocket(req)`: claims the request's parked upgrade (`upgrade_key` from
/// `velt_rt_http_req_upgrade`) → `IoResult<VeltWsAccept>`; `EINVAL` if the request is not a
/// WebSocket upgrade (no key, no `sec-websocket-key`) or was already accepted.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_ws_accept(
    upgrade_key: u64,
    sec_key: *const VeltStr,
    out: *mut IoResult<VeltWsAccept>,
) {
    let sec_key = (*sec_key).as_bytes();
    let r = match upgrade::claim(upgrade_key) {
        Some(on) if !sec_key.is_empty() => IoResult::ok(VeltWsAccept {
            ws: super::SOCKETS.insert(WsObj::upgrading(on)),
            response: crate::http::response::register(switching_protocols(sec_key)),
        }),
        _ => IoResult::err(invalid("not a WebSocket upgrade request".to_string())),
    };
    out.write(r);
}
