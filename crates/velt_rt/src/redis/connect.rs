//! Opening a Redis connection: TCP (or TLS through rustls with the shared `ring` configuration of
//! `crate::tls`), then the handshake (`AUTH`, `SELECT`) before the connection is handed to the
//! multiplexer or a subscriber.

use super::error::RedisErr;
use super::resp::{encode_command, Parser, Value};
use super::url::Target;
use rustls::pki_types::ServerName;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Any byte stream a connection runs on (TCP, or TLS over TCP).
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// An open connection's byte stream.
pub type Stream = Box<dyn Transport>;

/// Connecting (TCP + TLS + handshake) fails with `ETIMEDOUT` after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `reconnect` keeps trying (like ioredis' default of about 20 retries).
#[cfg(not(test))]
const RECONNECT_WINDOW: Duration = Duration::from_secs(10);
/// Unit tests exercise giving up without waiting the real window.
#[cfg(test)]
const RECONNECT_WINDOW: Duration = Duration::from_millis(300);

/// First retry delay of `reconnect`, doubled per attempt up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(50);
const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// Where to connect and the extra PEM CAs to trust for `rediss://` (kept by clients so that
/// `duplicate()` and `subscribe(client, …)` can open more connections like it).
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Parsed URL.
    pub target: Target,
    /// Extra trusted CA certificates (PEM; empty = built-in roots only).
    pub ca: Vec<u8>,
}

/// Open a stream to `ep` and run the handshake.
pub async fn open(ep: &Endpoint) -> Result<Stream, RedisErr> {
    match tokio::time::timeout(CONNECT_TIMEOUT, open_now(ep)).await {
        Ok(r) => r,
        Err(_) => Err(RedisErr {
            code: crate::result::code::TIMED_OUT,
            message: format!(
                "Redis connection to {}:{} timed out",
                ep.target.host, ep.target.port
            ),
        }),
    }
}

/// Open a replacement for a lost connection: retry with exponential backoff for up to
/// [`RECONNECT_WINDOW`], then fail with the last error. An error reply to the handshake (bad
/// password, unknown database) is final.
pub async fn reconnect(ep: &Endpoint) -> Result<Stream, RedisErr> {
    let start = tokio::time::Instant::now();
    let mut delay = FIRST_BACKOFF;
    loop {
        match open(ep).await {
            Ok(s) => return Ok(s),
            Err(e) if e.code == super::error::SERVER_ERROR => return Err(e),
            Err(e) if start.elapsed() + delay >= RECONNECT_WINDOW => return Err(e),
            Err(_) => {}
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

async fn open_now(ep: &Endpoint) -> Result<Stream, RedisErr> {
    let t = &ep.target;
    let tcp = TcpStream::connect((t.host.as_str(), t.port))
        .await
        .map_err(|e| RedisErr::io(&e))?;
    let _ = tcp.set_nodelay(true);
    let mut stream: Stream = if t.tls {
        let config = crate::tls::client_config(&ep.ca).map_err(RedisErr::invalid)?;
        let name = ServerName::try_from(t.host.clone())
            .map_err(|_| RedisErr::invalid(format!("invalid host name {:?}", t.host)))?;
        let tls = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|e| RedisErr::io(&e))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };
    handshake(&mut stream, t).await?;
    Ok(stream)
}

/// `AUTH [user] password` and `SELECT db` as the URL asks; an error reply fails the connect.
async fn handshake(stream: &mut Stream, t: &Target) -> Result<(), RedisErr> {
    let mut commands: Vec<Vec<String>> = vec![];
    if let Some(password) = &t.password {
        let mut auth = vec!["AUTH".to_string()];
        auth.extend(t.user.clone());
        auth.push(password.clone());
        commands.push(auth);
    }
    if t.db != 0 {
        commands.push(vec!["SELECT".to_string(), t.db.to_string()]);
    }
    if commands.is_empty() {
        return Ok(());
    }
    let mut payload = vec![];
    for c in &commands {
        encode_command(c, &mut payload);
    }
    write_all(stream, &payload).await?;
    for reply in read_replies(stream, commands.len()).await? {
        if let Value::Error(e) = reply {
            return Err(RedisErr::server(e));
        }
    }
    Ok(())
}

/// Write and flush (TLS streams buffer until flushed).
pub async fn write_all<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<(), RedisErr> {
    w.write_all(data).await.map_err(|e| RedisErr::io(&e))?;
    w.flush().await.map_err(|e| RedisErr::io(&e))
}

/// Read exactly `n` replies (nothing else is in flight during the handshake).
async fn read_replies(stream: &mut Stream, n: usize) -> Result<Vec<Value>, RedisErr> {
    let (mut parser, mut buf, mut out) = (Parser::default(), Vec::new(), Vec::new());
    while out.len() < n {
        if !fill(stream, &mut buf).await? {
            return Err(RedisErr::closed());
        }
        let used = parser.feed(&buf, &mut out).map_err(RedisErr::protocol)?;
        buf.drain(..used);
    }
    Ok(out)
}

/// Read more bytes into `buf`; false at end of stream.
pub async fn fill<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Result<bool, RedisErr> {
    buf.reserve(16 * 1024);
    let n = r.read_buf(buf).await.map_err(|e| RedisErr::io(&e))?;
    Ok(n > 0)
}
