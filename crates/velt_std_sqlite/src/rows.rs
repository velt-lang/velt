//! Reading an executing SQLite statement's rows for std's `JSON.parse<T[]>`, in the binary
//! form the JSON reader walks directly ([`velt_text::rows`]).
//!
//! SQLite has no boolean type, so booleans come back as 0/1. A column whose declared type is
//! `BOOL` or `BOOLEAN` (a table column read directly, not an expression) is written as
//! `true`/`false` when it holds 0 or 1, so it decodes into a `bool` field; any other column
//! stays a number.

use super::error::DbError;
use parking_lot::Mutex;
use rusqlite::types::ValueRef;
use rusqlite::{Statement, StatementStatus};
use velt_text::rows::{encode_names, BinRows};
use velt_text::Out;

/// What a prepared statement's rows look like: the column names (encoded for the rows header)
/// and which columns are declared boolean. Asking SQLite costs a call per column (each a UTF-8
/// check of the name), so a statement keeps them until SQLite compiles it anew.
pub struct Shape {
    /// SQLite's count of recompilations (a schema change) when the shape was read.
    reprepared: i32,
    count: usize,
    names: Vec<u8>,
    bools: Vec<bool>,
}

/// A statement's [`Shape`], once known.
pub type ShapeCell = Mutex<Option<Shape>>;

impl Shape {
    fn of(stmt: &Statement<'_>) -> Shape {
        let count = stmt.column_count();
        Shape {
            reprepared: stmt.get_status(StatementStatus::RePrepare),
            count,
            // A name SQLite cannot give (out of memory) is empty.
            names: encode_names((0..count).map(|i| stmt.column_name(i).unwrap_or(""))),
            bools: declared_bools(stmt),
        }
    }
}

/// Whether `stmt` has never run: just compiled (also anew, after the connection's cache let it
/// go), so a shape kept from before may not be its own. Asked before it steps (the reset at the
/// end of its rows counts a run).
fn fresh(stmt: &Statement<'_>) -> bool {
    stmt.get_status(StatementStatus::Run) == 0
}

/// The shape of `stmt`, which has stepped (so SQLite recompiled it if the schema changed),
/// from `cell` while it still applies.
fn shape<'c>(
    stmt: &Statement<'_>,
    fresh: bool,
    cell: &'c ShapeCell,
) -> parking_lot::MappedMutexGuard<'c, Shape> {
    let mut kept = cell.lock();
    let applies = kept
        .as_ref()
        .is_some_and(|s| !fresh && s.reprepared == stmt.get_status(StatementStatus::RePrepare));
    if !applies {
        *kept = Some(Shape::of(stmt));
    }
    parking_lot::MutexGuard::map(kept, |s| s.as_mut().expect("ICE: shape just set"))
}

/// Step `stmt` (already bound) and encode its rows into `out`: every row, or with `first_only`
/// the first row alone (nothing at all if there is none; the statement is not stepped further).
/// `cell` keeps the statement's [`Shape`].
pub fn read<O: Out>(
    stmt: &mut Statement<'_>,
    first_only: bool,
    out: O,
    cell: &ShapeCell,
) -> Result<O, DbError> {
    let fresh = fresh(stmt);
    let mut rows = stmt.raw_query();
    let Some(row) = rows.next()? else {
        drop(rows);
        if first_only {
            // No row is an empty result.
            return Ok(out);
        }
        let shape = shape(stmt, fresh, cell);
        return Ok(BinRows::with_names(out, &shape.names, false).finish());
    };
    let shape = shape(row.as_ref(), fresh, cell);
    let mut w = BinRows::with_names(out, &shape.names, first_only);
    write_row(&mut w, row, &shape)?;
    if !first_only {
        while let Some(row) = rows.next()? {
            write_row(&mut w, row, &shape)?;
        }
    }
    Ok(w.finish())
}

/// One row's cells. SQLite has no booleans: a 0 or 1 in a column declared `BOOL`/`BOOLEAN` is
/// written as a boolean.
fn write_row<O: Out>(
    w: &mut BinRows<O>,
    row: &rusqlite::Row<'_>,
    shape: &Shape,
) -> Result<(), DbError> {
    for i in 0..shape.count {
        match row.get_ref(i)? {
            ValueRef::Null => w.null(),
            ValueRef::Integer(v @ (0 | 1)) if shape.bools[i] => w.bool(v == 1),
            ValueRef::Integer(v) => w.int(v),
            ValueRef::Real(v) => w.float(v),
            ValueRef::Text(t) => w.text(t),
            ValueRef::Blob(b) => w.bytes(b),
        }
    }
    w.end_row();
    Ok(())
}

/// For every column of `stmt`, whether it is declared `BOOL` or `BOOLEAN`.
fn declared_bools(stmt: &Statement<'_>) -> Vec<bool> {
    stmt.columns()
        .iter()
        .map(|c| is_bool_type(c.decl_type()))
        .collect()
}

fn is_bool_type(decl: Option<&str>) -> bool {
    decl.is_some_and(|t| t.eq_ignore_ascii_case("BOOLEAN") || t.eq_ignore_ascii_case("BOOL"))
}

/// Step `stmt` (already bound) to completion, discarding any rows. A statement compiled anew
/// forgets the shape in `cell`, which may be another compilation's.
pub fn drain(stmt: &mut Statement<'_>, cell: &ShapeCell) -> Result<(), DbError> {
    if fresh(stmt) {
        *cell.lock() = None;
    }
    let mut rows = stmt.raw_query();
    while rows.next()?.is_some() {}
    Ok(())
}
