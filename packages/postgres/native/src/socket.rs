//! Opening a connection: the socket (TCP, or a Unix socket for a `host=/dir`), wrapped in the
//! connection's [`WireStream`], then tokio-postgres' startup over it (`Config::connect_raw`).
//!
//! The driver opens the socket itself because tokio-postgres' own `connect` gives no way to
//! wrap the stream. The socket is the runtime's own stream (`velt_native::net::TcpStream`): on
//! Velt's scheduler registered with the connecting worker's reactor, so a query and its reply
//! stay on that worker. Hosts (or `hostaddr`s) are tried in order, each with its port (or the
//! single port, default 5432); the runtime tries each address a name resolves to;
//! `connect_timeout` bounds each host; TCP gets `TCP_NODELAY` and the keepalive settings.
//! `target_session_attrs` and `load_balance_hosts` are not supported (the first host that
//! accepts is used).

use super::error::PgError;
use super::tls::{PgTls, PgTlsStream};
use super::wire::{Wire, WireStream};
use socket2::TcpKeepalive;
use std::future::Future;
use std::io;
use std::time::Duration;
use tokio_postgres::config::Host;
use tokio_postgres::{Client, Config, Connection};
use velt_native::net::TcpStream;

/// The plain connection under TLS (if any): the runtime's stream, over TCP or a Unix socket.
pub type RawSocket = TcpStream;

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
            // The runtime resolves the name (off the workers) and tries each address.
            let addr = match host.parse::<std::net::IpAddr>() {
                Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
                Err(_) => format!("{host}:{port}"),
            };
            let stream = within(timeout, TcpStream::connect(&addr)).await?;
            tune(config, &stream).map_err(|e| PgError::io(&e))?;
            Ok(stream)
        }
        #[cfg(unix)]
        Target::Unix(path) => connect_unix(&path)
            .and_then(TcpStream::from_unix_stream)
            .map_err(|e| PgError::io(&e)),
    }
}

/// Connects to a Unix socket without waiting: a local socket connects at once or fails (a full
/// listen backlog is `EAGAIN` on Linux, which libpq reports as well), where a blocking connect
/// would hold the worker until the server accepts. A connect still in progress completes
/// before the stream's first write does.
#[cfg(unix)]
pub(crate) fn connect_unix(path: &std::path::Path) -> io::Result<std::os::unix::net::UnixStream> {
    use socket2::{Domain, SockAddr, Socket, Type};
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    match socket.connect(&SockAddr::unix(path)?) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
        Err(e) => return Err(e),
    }
    Ok(std::os::unix::net::UnixStream::from(
        std::os::fd::OwnedFd::from(socket),
    ))
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
        socket2::SockRef::from(&stream.socket()).set_tcp_keepalive(&keepalive)?;
    }
    Ok(())
}

/// `op`, failing with `ETIMEDOUT` after `timeout`.
async fn within<T>(
    timeout: Option<Duration>,
    op: impl Future<Output = io::Result<T>>,
) -> Result<T, PgError> {
    let r = match timeout {
        Some(t) => velt_native::time::timeout(t, op).await.unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection timed out",
            ))
        }),
        None => op.await,
    };
    r.map_err(|e| PgError::io(&e))
}
