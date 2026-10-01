//! Opening a connection: the socket (TCP, or a Unix socket for a `host=/dir`), wrapped in the
//! connection's [`WireStream`], then tokio-postgres' startup over it (`Config::connect_raw`).
//!
//! The runtime opens the socket itself because tokio-postgres' own `connect` gives no way to
//! wrap the stream. It follows tokio-postgres' rules: hosts (or `hostaddr`s) are tried in order,
//! each with its port (or the single port, default 5432), every resolved address in turn;
//! `connect_timeout` bounds each attempt; TCP gets `TCP_NODELAY` and the keepalive settings.
//! `target_session_attrs` and `load_balance_hosts` are not supported (the first host that
//! accepts is used).

use super::error::PgError;
use super::tls::{PgTls, PgTlsStream};
use super::wire::{Wire, WireStream};
use socket2::{SockRef, TcpKeepalive};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio_postgres::config::Host;
use tokio_postgres::{Client, Config, Connection};

/// The plain connection under TLS (if any).
pub enum RawSocket {
    /// TCP.
    Tcp(TcpStream),
    /// A Unix domain socket.
    #[cfg(unix)]
    Unix(UnixStream),
}

/// What tokio-postgres drives: the wire over the socket, or over TLS on it.
pub type PgConnection = Connection<WireStream<RawSocket>, WireStream<PgTlsStream>>;

/// Where one attempt connects.
enum Target {
    Tcp(String),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

/// Connect to the first host in `config` that accepts: the client, its connection task and
/// the connection's wire.
pub async fn connect(
    config: &Config,
    tls: &PgTls,
) -> Result<(Client, PgConnection, Wire), PgError> {
    let hosts = config.get_hosts();
    let addrs = config.get_hostaddrs();
    let ports = config.get_ports();
    let count = hosts.len().max(addrs.len());
    if count == 0 {
        return Err(PgError::invalid("the connection string names no host"));
    }
    let mut last = None;
    for i in 0..count {
        let port = ports.get(i).or(ports.first()).copied().unwrap_or(5432);
        let hostname = match hosts.get(i) {
            Some(Host::Tcp(name)) => name.clone(),
            _ => String::new(),
        };
        let target = match (addrs.get(i), hosts.get(i)) {
            (Some(ip), _) => Target::Tcp(ip.to_string()),
            (None, Some(Host::Tcp(name))) => Target::Tcp(name.clone()),
            #[cfg(unix)]
            (None, Some(Host::Unix(dir))) => Target::Unix(dir.join(format!(".s.PGSQL.{port}"))),
            (None, None) => continue,
        };
        match attempt(config, tls, &hostname, target, port).await {
            Ok(r) => return Ok(r),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| PgError::invalid("the connection string names no host")))
}

/// One host: open the socket, then run the startup over the wire.
async fn attempt(
    config: &Config,
    tls: &PgTls,
    hostname: &str,
    target: Target,
    port: u16,
) -> Result<(Client, PgConnection, Wire), PgError> {
    let socket = open(config, target, port).await?;
    let wire = Wire::new();
    let stream = WireStream::new(socket, wire.shared());
    let (client, connection) = config.connect_raw(stream, tls.for_host(hostname)).await?;
    Ok((client, connection, wire))
}

async fn open(config: &Config, target: Target, port: u16) -> Result<RawSocket, PgError> {
    let timeout = config.get_connect_timeout().copied();
    match target {
        Target::Tcp(host) => {
            let addrs: Vec<_> = within(timeout, tokio::net::lookup_host((host.as_str(), port)))
                .await?
                .collect();
            let mut last = None;
            for addr in addrs {
                match within(timeout, TcpStream::connect(addr)).await {
                    Ok(stream) => {
                        tune(config, &stream).map_err(|e| PgError::io(&e))?;
                        return Ok(RawSocket::Tcp(stream));
                    }
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| {
                PgError::new("ENOTFOUND", format!("could not resolve host \"{host}\""))
            }))
        }
        #[cfg(unix)]
        Target::Unix(path) => Ok(RawSocket::Unix(
            within(timeout, UnixStream::connect(path)).await?,
        )),
    }
}

/// `TCP_NODELAY` (requests are small and latency-bound) and the configured keepalive.
fn tune(config: &Config, stream: &TcpStream) -> io::Result<()> {
    stream.set_nodelay(true)?;
    if config.get_keepalives() {
        let mut keepalive = TcpKeepalive::new().with_time(config.get_keepalives_idle());
        #[cfg(not(target_os = "openbsd"))]
        if let Some(interval) = config.get_keepalives_interval() {
            keepalive = keepalive.with_interval(interval);
        }
        SockRef::from(stream).set_tcp_keepalive(&keepalive)?;
    }
    Ok(())
}

/// `op`, failing with `ETIMEDOUT` after `timeout`.
async fn within<T>(
    timeout: Option<Duration>,
    op: impl Future<Output = io::Result<T>>,
) -> Result<T, PgError> {
    let r = match timeout {
        Some(t) => tokio::time::timeout(t, op).await.unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection timed out",
            ))
        }),
        None => op.await,
    };
    r.map_err(|e| PgError::io(&e))
}

impl AsyncRead for RawSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RawSocket::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(unix)]
            RawSocket::Unix(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for RawSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            RawSocket::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(unix)]
            RawSocket::Unix(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RawSocket::Tcp(s) => Pin::new(s).poll_flush(cx),
            #[cfg(unix)]
            RawSocket::Unix(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            RawSocket::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(unix)]
            RawSocket::Unix(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}
