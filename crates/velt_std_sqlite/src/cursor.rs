//! A statement's rows as a live cursor (`velt_text::rows_cursor`, native_abi.md "Row cursors"):
//! std's decoder reads each value from the stepped statement into the Velt value it builds,
//! instead of from a buffer the rows were written into first.
//!
//! The cursor holds the connection's lock while std decodes (a synchronous call: nothing else
//! of std's runs meanwhile), and releases it when std gives the cursor back ([`close`]), which
//! reports a failure SQLite met while stepping.

use super::connection::Locked;
use super::error::DbError;
use super::raw::RawStmt;
use super::rows::{self, Shape};
use super::statement::Prepared;
use std::sync::Arc;
use velt_text::rows_cursor::{Cell, RawCursor, RowCursor};

/// The rows of a stepped statement.
struct SqliteCursor {
    prepared: Arc<Prepared>,
    shape: Arc<Shape>,
    single: bool,
    /// The first row was stepped when the cursor was made and not handed out yet.
    pending: bool,
    done: bool,
    failure: Option<DbError>,
    /// Dropped last: the lock under which the statement is used.
    locked: Locked,
}

/// What std holds: the cursor's ABI form first (its address is the handle), then the cursor.
#[repr(C)]
struct Owned {
    raw: RawCursor,
    cursor: SqliteCursor,
}

impl SqliteCursor {
    fn stmt(&self) -> &RawStmt {
        &self.prepared.stmt
    }
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
        match self.stmt().step() {
            Ok(true) => true,
            Ok(false) => {
                self.done = true;
                false
            }
            Err(e) => {
                self.failure = Some(e);
                self.done = true;
                false
            }
        }
    }

    fn cell(&self, col: usize) -> Cell<'_> {
        if self.done {
            return Cell::Null;
        }
        // SAFETY: on a row (not done); `Shape::cell` checks `col`.
        unsafe { self.shape.cell(self.stmt(), col) }
    }
}

impl Drop for SqliteCursor {
    fn drop(&mut self) {
        // Still under the lock (dropped after this).
        self.stmt().finish();
        if let Some(e) = &self.failure {
            self.locked.note(e.clone());
        }
    }
}

/// Binds the parameters to `prepared`'s statement on the locked connection (`bind`), steps it
/// to its first row and makes a cursor over its rows (with `single`, the first row alone): the
/// cursor's handle for std, or 0 when `single` found no row.
pub(crate) fn open(
    mut locked: Locked,
    prepared: &Arc<Prepared>,
    single: bool,
    bind: impl FnOnce(&RawStmt) -> Result<(), DbError>,
) -> Result<u64, DbError> {
    let first = locked
        .conn()
        .and_then(|_| bind(&prepared.stmt))
        .and_then(|()| prepared.stmt.step());
    let pending = match first {
        Ok(on) => on,
        Err(e) => {
            prepared.stmt.finish();
            locked.note(e.clone());
            return Err(e);
        }
    };
    if single && !pending {
        prepared.stmt.finish();
        return Ok(0);
    }
    // After the first step: SQLite recompiled the statement if the schema changed.
    let shape = rows::shape(&prepared.stmt, &prepared.shape);
    let owned = Box::into_raw(Box::new(Owned {
        // Set below, once the cursor has its address.
        raw: RawCursor::new(std::ptr::null_mut::<SqliteCursor>()),
        cursor: SqliteCursor {
            prepared: prepared.clone(),
            shape,
            single,
            pending,
            done: !pending,
            failure: None,
            locked,
        },
    }));
    // SAFETY: just made; the cursor stays in the box until `close`.
    unsafe { (*owned).raw = RawCursor::new(&mut (*owned).cursor) };
    Ok(owned as u64)
}

/// Releases a cursor [`open`] made (0 is a no-op), and with it the connection's lock; the
/// failure SQLite met while stepping, if any (counted against the connection).
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
