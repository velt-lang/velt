//! A database: one SQLite connection behind a mutex, and the connection-level operations
//! (open, close, exec, pragma, transaction state) that [`crate::abi`] exports.
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
use super::raw::RawStmt;
use super::statement::Prepared;
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags};
use std::sync::Arc;
use std::time::Duration;
use velt_native::handles::{Key, Registry};

/// The SQL texts a connection keeps compiled ([`DbObj::prepared`]) beyond those a statement
/// handle uses.
const STATEMENT_CACHE: usize = 256;

/// A connection and its error bookkeeping (see the module docs).
pub struct DbObj {
    state: Mutex<ConnState>,
}

struct ConnState {
    /// The SQL compiled on this connection, by its text as given (trimmed). Dropped before the
    /// connection: their statements are finalized first.
    prepared: rustc_hash::FxHashMap<Box<[u8]>, Arc<Prepared>>,
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
                prepared: Default::default(),
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

    /// The [`Prepared`] of `sql` (exactly one statement): the one made before for this text, or
    /// a new one once SQLite compiled it (a syntax error fails here). Bytes that are not UTF-8
    /// (a lone surrogate) become U+FFFD.
    pub fn prepared(&self, sql: &[u8]) -> Result<Arc<Prepared>, DbError> {
        let mut state = self.state.lock();
        let state = &mut *state;
        let Some(conn) = &state.conn else {
            let e = DbError::closed("database connection");
            state.note(e.clone());
            return Err(e);
        };
        if let Some(p) = state.prepared.get(sql) {
            return Ok(p.clone());
        }
        let text = String::from_utf8_lossy(sql);
        let text = text.trim();
        // rusqlite checks the SQL and words its errors (more than one statement, a syntax
        // error's offset); the statement kept is compiled after.
        let compiled = conn
            .prepare(text)
            .map(drop)
            .map_err(DbError::from)
            .and_then(|()| RawStmt::prepare(conn, text));
        let stmt = match compiled {
            Ok(stmt) => stmt,
            Err(e) => {
                state.note(e.clone());
                return Err(e);
            }
        };
        if state.prepared.len() >= STATEMENT_CACHE {
            // Forget the SQL no handle uses (a handle keeps its own).
            state.prepared.retain(|_, p| Arc::strong_count(p) > 1);
        }
        let p = Arc::new(Prepared::new(stmt));
        state.prepared.insert(sql.into(), p.clone());
        Ok(p)
    }

    /// The connection, locked until the result is dropped (a row cursor holds it while std
    /// reads the rows).
    pub(crate) fn lock_owned(self: &Arc<Self>) -> Locked {
        let guard = self.state.lock();
        Locked {
            // SAFETY: the guard borrows `self.state`, which lives as long as the `Arc` the
            // result keeps (and drops after the guard).
            guard: unsafe {
                std::mem::transmute::<
                    parking_lot::MutexGuard<'_, ConnState>,
                    parking_lot::MutexGuard<'static, ConnState>,
                >(guard)
            },
            _db: self.clone(),
        }
    }

    /// Record a failure that happened outside the runtime (row decoding in std).
    pub fn note_error(&self, e: DbError) {
        self.state.lock().note(e);
    }
}

/// A connection locked for as long as this lives ([`DbObj::lock_owned`]).
pub(crate) struct Locked {
    // Dropped first: it borrows the object `_db` keeps alive.
    guard: parking_lot::MutexGuard<'static, ConnState>,
    _db: Arc<DbObj>,
}

impl Locked {
    /// The open connection, valid while this lives (`close` waits for the lock); a failure
    /// ("not open") is counted.
    pub(crate) fn conn(&mut self) -> Result<&'static Connection, DbError> {
        match &self.guard.conn {
            // SAFETY: the connection stays in place while the lock is held, which `self` does
            // until it is dropped; the cursor using it drops what borrows it first.
            Some(c) => Ok(unsafe { &*(c as *const Connection) }),
            None => {
                let e = DbError::closed("database connection");
                self.guard.note(e.clone());
                Err(e)
            }
        }
    }

    /// Count a failure against the connection.
    pub(crate) fn note(&mut self, e: DbError) {
        self.guard.note(e);
    }
}

impl ConnState {
    fn note(&mut self, e: DbError) {
        self.errors += 1;
        self.last_error = Some(e);
    }
}

/// `open(path, opts)`: a new database handle. `path` may be `":memory:"` or a `file:` URI;
/// `timeout_ms` is the busy timeout (how long to wait for another connection's lock).
pub fn open(
    path: &str,
    readonly: bool,
    create: bool,
    timeout_ms: u32,
) -> Result<DbHandle, DbError> {
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
    Ok(DATABASES.insert(DbObj::new(conn)))
}

/// The object behind a handle, or the "not open" error for a closed or null handle.
pub(super) fn obj(db: DbHandle) -> Result<Arc<DbObj>, DbError> {
    DATABASES
        .get(db)
        .ok_or_else(|| DbError::closed("database connection"))
}

/// Close the connection (its cached statements are finalized) and release the handle; the null
/// or a closed handle is a no-op. Statements of this connection fail from now on.
pub fn close(db: DbHandle) -> Result<(), DbError> {
    let Some(obj) = DATABASES.remove(db) else {
        return Ok(());
    };
    let conn = {
        let mut state = obj.state.lock();
        // Handles may still hold some: their statements are finalized all the same, and they
        // fail with "not open" from now on.
        for p in state.prepared.values() {
            p.stmt.finalize();
        }
        state.prepared.clear();
        state.conn.take()
    };
    match conn.map(Connection::close) {
        Some(Err((_, e))) => Err(DbError::from(e)),
        _ => Ok(()),
    }
}

/// Run one or more `;`-separated statements without parameters (schema scripts).
pub fn exec(db: DbHandle, sql: &str) -> Result<(), DbError> {
    obj(db).and_then(|o| o.with(|c| Ok(c.execute_batch(sql)?)))
}

/// First column of the first row of `PRAGMA <source>` as text (`""` if there is no row or the
/// value is NULL or a blob): `"wal"` for `journal_mode = WAL`, `"0"` for `user_version`.
pub fn pragma(db: DbHandle, source: &str) -> Result<String, DbError> {
    obj(db).and_then(|o| o.with(|c| pragma_on(c, source)))
}

fn pragma_on(conn: &Connection, source: &str) -> Result<String, DbError> {
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
            velt_text::fmt::push_f64(&mut v, f);
            String::from_utf8_lossy(&v).into_owned()
        }
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Null | ValueRef::Blob(_) => String::new(),
    })
}

/// Whether a transaction is open (the connection is not in autocommit mode); false once closed.
pub fn in_transaction(db: DbHandle) -> bool {
    let Some(obj) = DATABASES.get(db) else {
        return false;
    };
    let state = obj.state.lock();
    state.conn.as_ref().is_some_and(|c| !c.is_autocommit())
}

/// How many operations on this connection have failed so far.
pub fn error_count(db: DbHandle) -> u64 {
    DATABASES.get(db).map_or(0, |obj| obj.state.lock().errors)
}

/// Set the failure count back to `count` (a value [`error_count`] returned): a nested
/// `transaction` that rolled back and rethrew its failure hands it to the caller as a thrown
/// error, so the enclosing transaction no longer counts it.
pub fn reset_error_count(db: DbHandle, count: u64) {
    if let Some(obj) = DATABASES.get(db) {
        obj.state.lock().errors = count;
    }
}

/// The most recent failure on this connection, if any.
pub fn last_error(db: DbHandle) -> Option<DbError> {
    DATABASES
        .get(db)
        .and_then(|obj| obj.state.lock().last_error.clone())
}
