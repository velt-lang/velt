//! `VeltSqliteDb`: one SQLite connection behind a mutex, and the connection-level ABI (open,
//! close, exec, pragma, transaction state).
//!
//! The connection lives in `Mutex<Option<Connection>>`: statements hold an `Arc` to the
//! [`DbObj`] and lock it per call, so any task on any thread may use them; `close` takes the
//! connection out, after which every call fails with "The database connection is not open"
//! (statements stay valid handles until they are closed themselves).
//!
//! The object also counts failed operations and keeps the last error: `Database.transaction`
//! (std) compares the count before and after its callback, so a failure the callback caught
//! still rolls the transaction back (Velt closures cannot throw yet). A nested transaction
//! that rolled back resets the count, so its failure only undoes its own savepoint.

use super::error::DbError;
use crate::registry::{Key, Registry};
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::sync::Arc;
use std::time::Duration;

/// Prepared statements kept per connection: `Statement` handles re-find theirs by SQL text on
/// every call, so this bounds how many distinct statements stay compiled.
const STATEMENT_CACHE: usize = 256;

/// A connection and its error bookkeeping (see the module docs).
pub struct DbObj {
    state: Mutex<ConnState>,
}

struct ConnState {
    conn: Option<Connection>,
    errors: u64,
    last_error: Option<DbError>,
}

/// Opaque handle: a key into [`DATABASES`] (std's `Database` is a Copy struct, so after
/// `close()` through one copy every copy must fail cleanly instead of reaching freed memory).
pub type DbHandle = Key<DbObj>;

static DATABASES: Registry<DbObj> = Registry::new();

impl DbObj {
    fn new(conn: Connection) -> DbObj {
        DbObj {
            state: Mutex::new(ConnState {
                conn: Some(conn),
                errors: 0,
                last_error: None,
            }),
        }
    }

    /// Run `f` on the open connection under the lock; a failure (including "not open") is
    /// counted and remembered as the last error.
    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> Result<R, DbError>) -> Result<R, DbError> {
        let mut state = self.state.lock();
        let r = match &state.conn {
            Some(conn) => f(conn),
            None => Err(DbError::closed("database connection")),
        };
        if let Err(e) = &r {
            state.note(e.clone());
        }
        r
    }

    /// Record a failure that happened outside the runtime (row decoding in std).
    pub fn note_error(&self, e: DbError) {
        self.state.lock().note(e);
    }
}

impl ConnState {
    fn note(&mut self, e: DbError) {
        self.errors += 1;
        self.last_error = Some(e);
    }
}

fn open(path: &str, readonly: bool, create: bool, timeout_ms: u32) -> Result<DbObj, DbError> {
    let mut flags = OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if readonly {
        flags |= OpenFlags::SQLITE_OPEN_READ_ONLY;
    } else {
        flags |= OpenFlags::SQLITE_OPEN_READ_WRITE;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
    }
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_millis(timeout_ms as u64))?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(DbObj::new(conn))
}

/// A string argument as UTF-8 for SQLite (one U+FFFD per lone surrogate, #377).
unsafe fn text<'a>(s: *const VeltStr) -> std::borrow::Cow<'a, str> {
    (*s).text_lossy()
}

/// The object behind a handle, or the "not open" error for a closed or null handle.
pub(super) fn obj(db: DbHandle) -> Result<Arc<DbObj>, DbError> {
    DATABASES
        .get(db)
        .ok_or_else(|| DbError::closed("database connection"))
}

/// `open(path, opts)` → `IoResult<VeltSqliteDb>`. `path` may be `":memory:"` or a `file:` URI;
/// `timeout_ms` is the busy timeout (how long to wait for another connection's lock).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_open(
    path: *const VeltStr,
    readonly: u8,
    create: u8,
    timeout_ms: u32,
    out: *mut IoResult<DbHandle>,
) {
    let r = match open(&text(path), readonly != 0, create != 0, timeout_ms) {
        Ok(obj) => IoResult::ok(DATABASES.insert(obj)),
        Err(e) => IoResult::err(e.to_velt()),
    };
    out.write(r);
}

/// Close the connection (its cached statements are finalized) and release the handle; the null
/// handle is a no-op. Statements of this connection fail from now on.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_close(db: DbHandle, out: *mut VeltErr) {
    let Some(obj) = DATABASES.remove(db) else {
        out.write(VeltErr::ok());
        return;
    };
    let conn = obj.state.lock().conn.take();
    let status = match conn.map(Connection::close) {
        Some(Err((_, e))) => DbError::from(e).to_velt(),
        _ => VeltErr::ok(),
    };
    out.write(status);
}

/// Run one or more `;`-separated statements without parameters (schema scripts).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_exec(db: DbHandle, sql: *const VeltStr, out: *mut VeltErr) {
    let r = obj(db).and_then(|o| o.with(|c| Ok(c.execute_batch(&text(sql))?)));
    out.write(r.map_or_else(|e| e.to_velt(), |_| VeltErr::ok()));
}

/// First column of the first row of `PRAGMA <source>` as text (`""` if there is no row or the
/// value is NULL or a blob): `"wal"` for `journal_mode = WAL`, `"0"` for `user_version`.
fn pragma(conn: &Connection, source: &str) -> Result<String, DbError> {
    let mut stmt = conn.prepare(&format!("PRAGMA {source}"))?;
    let mut rows = stmt.raw_query();
    let Some(row) = rows.next()? else {
        return Ok(String::new());
    };
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(0)? {
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            let mut v = Vec::new();
            crate::fmt::push_f64(&mut v, f);
            String::from_utf8_lossy(&v).into_owned()
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Null | ValueRef::Blob(_) => String::new(),
    })
}

/// `db.pragma(source)` → `IoResult<string>` (see [`pragma`]).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_pragma(
    db: DbHandle,
    source: *const VeltStr,
    out: *mut IoResult<VeltStr>,
) {
    let r = obj(db).and_then(|o| o.with(|c| pragma(c, &text(source))));
    out.write(match r {
        Ok(s) => IoResult::ok(VeltStr::from_vec(s.into_bytes())),
        Err(e) => IoResult::err(e.to_velt()),
    });
}

/// Whether a transaction is open (the connection is not in autocommit mode); 0 once closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_in_transaction(db: DbHandle) -> u8 {
    let Some(obj) = DATABASES.get(db) else {
        return 0;
    };
    let state = obj.state.lock();
    state.conn.as_ref().is_some_and(|c| !c.is_autocommit()) as u8
}

/// How many operations on this connection have failed so far.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_error_count(db: DbHandle) -> u64 {
    DATABASES.get(db).map_or(0, |obj| obj.state.lock().errors)
}

/// Set the failure count back to `count` (a value `velt_rt_sqlite_error_count` returned): a
/// nested `transaction` that rolled back and rethrew its failure hands it to the caller as a
/// thrown error, so the enclosing transaction no longer counts it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_reset_error_count(db: DbHandle, count: u64) {
    if let Some(obj) = DATABASES.get(db) {
        obj.state.lock().errors = count;
    }
}

/// The most recent failure on this connection as a failed status (code 0 if there was none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_last_error(db: DbHandle, out: *mut VeltErr) {
    let last = DATABASES
        .get(db)
        .and_then(|obj| obj.state.lock().last_error.clone());
    out.write(last.map_or_else(VeltErr::ok, |e| e.to_velt()));
}
