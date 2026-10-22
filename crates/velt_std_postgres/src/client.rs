//! `VeltPgClient`: a connection as std's `Client` holds it, and the client ABI.
//!
//! The handle is a registry key (`velt_native::handles`: std's `Client` is a Copy struct, so after
//! `close()` through one copy every copy fails with `ECLOSED` instead of reaching freed
//! memory); operations in flight keep a clone of the connection, so `close` is safe at any
//! time. A client from `pool.connect()` gives its connection back to the
//! pool on `close` (unless it is broken or inside a transaction, when it is dropped instead).
//!
//! The object counts failed operations and keeps the recent errors: `Client.transaction`
//! (std) compares the count before and after its callback, so a failure the callback caught
//! still rolls the transaction back (Velt closures cannot throw yet), and it reports the
//! *first* failure since then (later ones are usually `25P02` "current transaction is
//! aborted").

use super::connection::Conn;
use super::error::PgError;
use super::pool::PoolObj;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;
use velt_native::handles::{Key, Registry};
use velt_native::{export, Error, Str, StrBuf};

/// Failures remembered for `transaction` (older ones are dropped).
const RECENT_ERRORS: usize = 32;

/// A pooled client's claim on its pool: the pool and the permit counting it against `max`.
pub struct Lease {
    /// The pool the connection returns to.
    pub pool: Arc<PoolObj>,
    /// Released when the client closes.
    pub permit: OwnedSemaphorePermit,
}

struct State {
    conn: Option<Arc<Conn>>,
    lease: Option<Lease>,
    /// Failed operations so far.
    errors: u64,
    /// `(sequence number, error)` of the most recent failures (sequence = `errors` after it).
    recent: Vec<(u64, PgError)>,
}

/// A client (see the module docs).
pub struct ClientObj {
    state: Mutex<State>,
}

/// Opaque handle (a key into [`CLIENTS`]).
pub type ClientHandle = Key<ClientObj>;

/// Open clients (standalone and pooled).
pub(super) static CLIENTS: Registry<ClientObj> = Registry::new();

impl ClientObj {
    /// A client over `conn`, pooled when `lease` is given.
    pub fn new(conn: Conn, lease: Option<Lease>) -> ClientObj {
        Self::from_arc(Arc::new(conn), lease)
    }

    /// A client over a shared connection (a pooled one).
    pub fn from_arc(conn: Arc<Conn>, lease: Option<Lease>) -> ClientObj {
        ClientObj {
            state: Mutex::new(State {
                conn: Some(conn),
                lease,
                errors: 0,
                recent: Vec::new(),
            }),
        }
    }

    /// The connection, or `ECLOSED` once the client was closed.
    pub(super) fn conn(&self) -> Result<Arc<Conn>, PgError> {
        self.state
            .lock()
            .conn
            .clone()
            .ok_or_else(|| PgError::closed("client"))
    }

    /// Record a failure.
    pub fn note(&self, e: PgError) {
        let mut s = self.state.lock();
        s.errors += 1;
        let seq = s.errors;
        if s.recent.len() == RECENT_ERRORS {
            s.recent.remove(0);
        }
        s.recent.push((seq, e));
    }

    /// The first failure after `count` failures (the oldest remembered one if it was
    /// dropped), or `None` if there was none.
    fn error_after(&self, count: u64) -> Option<PgError> {
        let s = self.state.lock();
        s.recent
            .iter()
            .find(|(seq, _)| *seq > count)
            .map(|(_, e)| e.clone())
    }

    /// Detach the connection: back to the pool if leased and reusable, else dropped.
    fn close(&self) {
        let (conn, lease) = {
            let mut s = self.state.lock();
            (s.conn.take(), s.lease.take())
        };
        if let (Some(conn), Some(lease)) = (conn, lease) {
            lease.pool.put_back(conn);
            drop(lease.permit);
        }
    }
}

/// A driver result as Velt receives it (the error as JSON, see `error`).
pub(super) fn io_result<T>(r: Result<T, PgError>) -> Result<T, Error> {
    r.map_err(|e| e.to_error())
}

/// Run `op` on the client's connection, counting a failure.
pub(super) async fn client_op<R, F, Fut>(client: u64, op: F) -> Result<R, Error>
where
    F: FnOnce(Arc<Conn>) -> Fut,
    Fut: Future<Output = Result<R, PgError>>,
{
    let Some(obj) = CLIENTS.get(Key::from_bits(client)) else {
        return io_result(Err(PgError::closed("client")));
    };
    let r = match obj.conn() {
        Ok(conn) => op(conn).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &r {
        obj.note(e.clone());
    }
    io_result(r)
}

/// `connect(url)` → `IoResult<VeltPgClient>`.
#[export(async)]
async fn velt_std_postgres__connect(url: String) -> Result<u64, Error> {
    let r = async {
        let config = super::config::parse(&url)?;
        let tls = super::tls::connector(&config).map_err(|e| PgError::new(super::error::TLS, e))?;
        let conn = super::connection::connect(&config, tls).await?;
        Ok(CLIENTS.insert(ClientObj::new(conn, None)).bits())
    };
    io_result(r.await)
}

/// `client.query(sql, params)` → `IoResult<string>`: rows as a JSON array, or with
/// `first_only` the first row's object (`""` if none).
#[export(async)]
async fn velt_std_postgres__query(
    client: u64,
    sql: Str,
    params: Str,
    first_only: bool,
) -> Result<Str, Error> {
    client_op(client, move |c| async move {
        let out = StrBuf::new();
        let json = c
            .query(&sql.to_str(), params.as_bytes(), first_only, out)
            .await?;
        Ok(json.finish())
    })
    .await
}

/// `client.execute(sql, params)` → `IoResult<i64>`: rows affected.
#[export(async)]
async fn velt_std_postgres__execute(client: u64, sql: Str, params: Str) -> Result<i64, Error> {
    client_op(client, move |c| async move {
        Ok(c.execute(&sql.to_str(), params.as_bytes()).await? as i64)
    })
    .await
}

/// `client.batch(sql)` → `IoStatus`: `;`-separated statements, no parameters.
#[export(async)]
async fn velt_std_postgres__batch(client: u64, sql: Str) -> Result<(), Error> {
    client_op(client, move |c| async move { c.batch(&sql.to_str()).await }).await
}

/// `client.begin()` → `IoResult<u32>`: the new transaction level (1 = `BEGIN`, deeper levels
/// are savepoints).
#[export(async)]
async fn velt_std_postgres__begin(client: u64) -> Result<u32, Error> {
    client_op(client, |c| async move { c.begin().await }).await
}

/// Commit (`commit`) or roll back transaction level `depth` → `IoStatus`.
#[export(async)]
async fn velt_std_postgres__end(client: u64, depth: u32, commit: bool) -> Result<(), Error> {
    client_op(client, move |c| async move { c.end(depth, commit).await }).await
}

/// Open transaction levels on the client's connection (0 when none or closed).
#[export]
fn velt_std_postgres__depth(client: u64) -> u32 {
    CLIENTS
        .get(Key::from_bits(client))
        .and_then(|o| o.conn().ok())
        .map_or(0, |c| c.depth())
}

/// How many operations on this client have failed so far.
#[export]
fn velt_std_postgres__error_count(client: u64) -> u64 {
    CLIENTS
        .get(Key::from_bits(client))
        .map_or(0, |o| o.state.lock().errors)
}

/// Set the failure count back to `count` (a nested transaction handed its failure to its
/// caller as a thrown error).
#[export]
fn velt_std_postgres__reset_error_count(client: u64, count: u64) {
    if let Some(o) = CLIENTS.get(Key::from_bits(client)) {
        let mut s = o.state.lock();
        s.errors = count;
        s.recent.retain(|(seq, _)| *seq <= count);
    }
}

/// The first failure after `count` failures, as a failed status (success if none).
#[export]
fn velt_std_postgres__error_after(client: u64, count: u64) -> Result<(), Error> {
    match CLIENTS
        .get(Key::from_bits(client))
        .and_then(|o| o.error_after(count))
    {
        Some(e) => Err(e.to_error()),
        None => Ok(()),
    }
}

/// Count a failure std found (a row that does not decode) against the client.
#[export]
fn velt_std_postgres__note_error(client: u64, code: &str, message: &str) {
    if let Some(o) = CLIENTS.get(Key::from_bits(client)) {
        o.note(PgError::new(code, message));
    }
}

/// Close the client and release the handle (a closed or null handle ⇒ no-op): a standalone
/// connection is closed, a pooled one returns to its pool.
#[export]
fn velt_std_postgres__close(client: u64) {
    if let Some(o) = CLIENTS.remove(Key::from_bits(client)) {
        o.close();
    }
}
