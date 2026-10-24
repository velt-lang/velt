//! A statement's rows as a live cursor (`velt_text::rows_cursor`, native_abi.md "Row cursors"):
//! std's decoder reads each value from the stepped statement into the Velt value it builds,
//! instead of from a buffer the rows were written into first.
//!
//! The cursor holds the connection's lock and the compiled statement while std decodes (a
//! synchronous call: nothing else of std's runs meanwhile), and releases both when std gives it
//! back ([`close`]), which reports a failure SQLite met while stepping.

use super::connection::Locked;
use super::error::DbError;
use super::rows::{self, Shape};
use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator;
use rusqlite::{CachedStatement, Rows};
use std::mem::ManuallyDrop;
use std::sync::Arc;
use velt_text::rows_cursor::{Cell, RawCursor, RowCursor};

/// The rows of a stepped statement.
struct SqliteCursor {
    /// Borrows `*stmt`; dropped first (it resets the statement).
    rows: ManuallyDrop<Rows<'static>>,
    /// Borrows the connection `locked` holds; dropped next (back into the statement cache).
    stmt: *mut CachedStatement<'static>,
    shape: Arc<Shape>,
    single: bool,
    /// The first row was stepped when the cursor was made and not handed out yet.
    pending: bool,
    done: bool,
    failure: Option<DbError>,
    /// Dropped last: the lock that keeps the connection in place.
    locked: ManuallyDrop<Locked>,
}

/// What std holds: the cursor's ABI form first (its address is the handle), then the cursor.
#[repr(C)]
struct Owned {
    raw: RawCursor,
    cursor: SqliteCursor,
}

impl RowCursor for SqliteCursor {
    fn header(&self) -> &[u8] {
        self.shape.header(self.single)
    }

    fn step(&mut self) -> bool {
        if std::mem::take(&mut self.pending) {
            return true;
        }
        if self.done {
            return false;
        }
        match self.rows.next() {
            Ok(Some(_)) => true,
            Ok(None) => {
                self.done = true;
                false
            }
            Err(e) => {
                self.failure = Some(DbError::from(e));
                self.done = true;
                false
            }
        }
    }

    fn cell(&self, col: usize) -> Cell<'_> {
        match self.rows.get() {
            Some(row) if col < self.shape.columns() => self.shape.cell(row, col),
            _ => Cell::Null,
        }
    }
}

impl Drop for SqliteCursor {
    fn drop(&mut self) {
        // SAFETY: in the order the fields borrow each other, each once.
        unsafe {
            ManuallyDrop::drop(&mut self.rows);
            drop(Box::from_raw(self.stmt));
            if let Some(e) = &self.failure {
                self.locked.note(e.clone());
            }
            ManuallyDrop::drop(&mut self.locked);
        }
    }
}

/// Binds `params` to `sql`'s statement on the locked connection, steps it to its first row and
/// makes a cursor over its rows (with `single`, the first row alone): the cursor's handle for
/// std, or 0 when `single` found no row. `bind` binds the parameters.
pub(crate) fn open(
    mut locked: Locked,
    sql: &str,
    cell: &rows::ShapeCell,
    single: bool,
    bind: impl FnOnce(&mut rusqlite::Statement<'_>) -> Result<(), DbError>,
) -> Result<u64, DbError> {
    let conn = locked.conn()?;
    let mut st = match conn.prepare_cached(sql).map_err(DbError::from) {
        Ok(st) => st,
        Err(e) => {
            locked.note(e.clone());
            return Err(e);
        }
    };
    if let Err(e) = bind(&mut st) {
        locked.note(e.clone());
        return Err(e);
    }
    let fresh = rows::fresh(&st);
    let stmt = Box::into_raw(Box::new(st));
    // SAFETY: `*stmt` stays at its address until the cursor drops it, after the rows.
    let mut rows: Rows<'static> = unsafe { (*stmt).raw_query() };
    let first = match rows.next() {
        Ok(Some(row)) => Ok(Some(rows::shape(row.as_ref(), fresh, cell))),
        Ok(None) => Ok(None),
        Err(e) => Err(DbError::from(e)),
    };
    let (shape, pending) = match first {
        Ok(Some(shape)) => (shape, true),
        Ok(None) if !single => {
            drop(rows);
            // SAFETY: the rows that borrowed it are gone; new ones borrow it from here on.
            let shape = rows::shape(unsafe { &*stmt }, fresh, cell);
            rows = unsafe { (*stmt).raw_query() };
            (shape, false)
        }
        r => {
            drop(rows);
            // SAFETY: as above; dropped once.
            drop(unsafe { Box::from_raw(stmt) });
            return match r {
                Err(e) => {
                    locked.note(e.clone());
                    Err(e)
                }
                _ => Ok(0),
            };
        }
    };
    let owned = Box::into_raw(Box::new(Owned {
        // Set below, once the cursor has its address.
        raw: RawCursor::new(std::ptr::null_mut::<SqliteCursor>()),
        cursor: SqliteCursor {
            rows: ManuallyDrop::new(rows),
            stmt,
            shape,
            single,
            pending,
            done: !pending,
            failure: None,
            locked: ManuallyDrop::new(locked),
        },
    }));
    // SAFETY: just made; the cursor stays in the box until `close`.
    unsafe { (*owned).raw = RawCursor::new(&mut (*owned).cursor) };
    Ok(owned as u64)
}

/// Releases a cursor [`open`] made (0 is a no-op): the statement and the connection's lock;
/// the failure SQLite met while stepping, if any (counted against the connection).
///
/// # Safety
/// `handle` came from [`open`] and is not used afterwards.
pub unsafe fn close(handle: u64) -> Result<(), DbError> {
    if handle == 0 {
        return Ok(());
    }
    let owned = Box::from_raw(handle as *mut Owned);
    let failure = owned.cursor.failure.clone();
    drop(owned);
    failure.map_or(Ok(()), Err)
}
