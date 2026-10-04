//! `VeltPgClient`: a connection as std's `Client` holds it, and the client ABI.
//!
//! The handle is a registry key (`crate::registry`: std's `Client` is a Copy struct, so after
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
use crate::registry::{Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

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

/// Owned text of a string argument (copied: the operation outlives the call).
pub(super) unsafe fn text(s: *const VeltStr) -> String {
    (*s).text_lossy().into_owned()
}

/// Owned bytes of a string argument.
pub(super) unsafe fn bytes(s: *const VeltStr) -> Vec<u8> {
    (*s).as_bytes().to_vec()
}

/// `IoResult` of a result, with the error as JSON (see `error`).
pub(super) fn io_result<T>(r: Result<T, PgError>) -> IoResult<T> {
    match r {
        Ok(v) => IoResult::ok(v),
        Err(e) => IoResult::err(e.to_velt()),
    }
}

/// Run `op` on the client's connection as a leaf future, counting a failure.
pub(super) unsafe fn client_op<R, F, Fut>(client: ClientHandle, op: F) -> *mut VeltFut
where
    R: Send + 'static,
    F: FnOnce(Arc<Conn>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<R, PgError>> + Send,
{
    let obj = CLIENTS.get(client);
    new_leaf(async move {
        let Some(obj) = obj else {
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
    })
}

/// `connect(url)` → `IoResult<VeltPgClient>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_connect(url: *const VeltStr) -> *mut VeltFut {
    let url = text(url);
    new_leaf(async move {
        let r = async {
            let config = super::config::parse(&url)?;
            let tls =
                super::tls::connector(&config).map_err(|e| PgError::new(super::error::TLS, e))?;
            let conn = super::connection::connect(&config, tls).await?;
            Ok(CLIENTS.insert(ClientObj::new(conn, None)))
        };
        io_result(r.await)
    })
}

/// `client.query(sql, params)` → `IoResult<string>`: rows as a JSON array, or with
/// `first_only` the first row's object (`""` if none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_query(
    client: ClientHandle,
    sql: *const VeltStr,
    params: *const VeltStr,
    first_only: u8,
) -> *mut VeltFut {
    let (sql, params) = (text(sql), bytes(params));
    client_op(client, move |c| async move {
        let json = c.query(&sql, &params, first_only != 0).await?;
        Ok(VeltStr::from_vec(json))
    })
}

/// `client.execute(sql, params)` → `IoResult<i64>`: rows affected.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_execute(
    client: ClientHandle,
    sql: *const VeltStr,
    params: *const VeltStr,
) -> *mut VeltFut {
    let (sql, params) = (text(sql), bytes(params));
    client_op(client, move |c| async move {
        Ok(c.execute(&sql, &params).await? as i64)
    })
}

/// `client.batch(sql)` → `VeltErr`: `;`-separated statements, no parameters.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_batch(
    client: ClientHandle,
    sql: *const VeltStr,
) -> *mut VeltFut {
    let sql = text(sql);
    status(client_op(
        client,
        move |c| async move { c.batch(&sql).await },
    ))
}

/// `client.begin()` → `IoResult<u32>`: the new transaction level (1 = `BEGIN`, deeper levels
/// are savepoints).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_begin(client: ClientHandle) -> *mut VeltFut {
    client_op(client, |c| async move { c.begin().await })
}

/// Commit (`commit != 0`) or roll back transaction level `depth` → `VeltErr`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_end(
    client: ClientHandle,
    depth: u32,
    commit: u8,
) -> *mut VeltFut {
    status(client_op(client, move |c| async move {
        c.end(depth, commit != 0).await
    }))
}

/// A `VeltErr`-result op: `IoResult<()>` has the same layout (a zero-sized value at +32).
fn status(f: *mut VeltFut) -> *mut VeltFut {
    const { assert!(std::mem::size_of::<IoResult<()>>() == std::mem::size_of::<VeltErr>()) };
    f
}

/// Open transaction levels on the client's connection (0 when none or closed).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_depth(client: ClientHandle) -> u32 {
    CLIENTS
        .get(client)
        .and_then(|o| o.conn().ok())
        .map_or(0, |c| c.depth())
}

/// How many operations on this client have failed so far.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_error_count(client: ClientHandle) -> u64 {
    CLIENTS.get(client).map_or(0, |o| o.state.lock().errors)
}

/// Set the failure count back to `count` (a nested transaction handed its failure to its
/// caller as a thrown error).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_reset_error_count(client: ClientHandle, count: u64) {
    if let Some(o) = CLIENTS.get(client) {
        let mut s = o.state.lock();
        s.errors = count;
        s.recent.retain(|(seq, _)| *seq <= count);
    }
}

/// The first failure after `count` failures, as a failed status (code 0 if none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_error_after(
    client: ClientHandle,
    count: u64,
    out: *mut VeltErr,
) {
    let e = CLIENTS.get(client).and_then(|o| o.error_after(count));
    out.write(e.map_or_else(VeltErr::ok, |e| e.to_velt()));
}

/// Count a failure std found (a row that does not decode) against the client.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_note_error(
    client: ClientHandle,
    code: *const VeltStr,
    message: *const VeltStr,
) {
    if let Some(o) = CLIENTS.get(client) {
        o.note(PgError::new(&text(code), text(message)));
    }
}

/// Close the client and release the handle (a closed or null handle ⇒ no-op): a standalone
/// connection is closed, a pooled one returns to its pool.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_close(client: ClientHandle) {
    if let Some(o) = CLIENTS.remove(client) {
        o.close();
    }
}
