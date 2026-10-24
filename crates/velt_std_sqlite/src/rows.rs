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
use std::sync::Arc;
use velt_text::rows::encode_names;
use velt_text::rows_cursor::Cell;

/// What a prepared statement's rows look like: the rows header (with the column names) and
/// which columns are declared boolean. Asking SQLite costs a call per column (each a UTF-8
/// check of the name), so a statement keeps them until SQLite compiles it anew.
pub struct Shape {
    /// SQLite's count of recompilations (a schema change) when the shape was read.
    reprepared: i32,
    count: usize,
    /// The rows header (`velt_text::rows`), for many rows and for a single one.
    rows_header: Vec<u8>,
    single_header: Vec<u8>,
    bools: Vec<bool>,
}

/// A statement's [`Shape`], once known.
pub type ShapeCell = Mutex<Option<Arc<Shape>>>;

impl Shape {
    fn of(stmt: &Statement<'_>) -> Shape {
        let count = stmt.column_count();
        // A name SQLite cannot give (out of memory) is empty.
        let names = encode_names((0..count).map(|i| stmt.column_name(i).unwrap_or("")));
        let header = |single: u8| [&[single][..], &names].concat();
        Shape {
            reprepared: stmt.get_status(StatementStatus::RePrepare),
            count,
            rows_header: header(0),
            single_header: header(1),
            bools: declared_bools(stmt),
        }
    }

    /// The rows header: `single`, the column count and names.
    pub fn header(&self, single: bool) -> &[u8] {
        match single {
            true => &self.single_header,
            false => &self.rows_header,
        }
    }

    /// Cell `col` of `row`. SQLite has no booleans: a 0 or 1 in a column declared
    /// `BOOL`/`BOOLEAN` is a boolean.
    pub fn cell<'r>(&self, row: &'r rusqlite::Row<'_>, col: usize) -> Cell<'r> {
        match row.get_ref(col) {
            Ok(ValueRef::Null) | Err(_) => Cell::Null,
            Ok(ValueRef::Integer(v @ (0 | 1))) if self.bools[col] => Cell::Bool(v == 1),
            Ok(ValueRef::Integer(v)) => Cell::Int(v),
            Ok(ValueRef::Real(v)) => Cell::Float(v),
            Ok(ValueRef::Text(t)) => Cell::Text(t),
            Ok(ValueRef::Blob(b)) => Cell::Blob(b),
        }
    }

    pub fn columns(&self) -> usize {
        self.count
    }
}

/// Whether `stmt` has never run: just compiled (also anew, after the connection's cache let it
/// go), so a shape kept from before may not be its own. Asked before it steps (the reset at the
/// end of its rows counts a run).
pub fn fresh(stmt: &Statement<'_>) -> bool {
    stmt.get_status(StatementStatus::Run) == 0
}

/// The shape of `stmt`, which has stepped (so SQLite recompiled it if the schema changed),
/// from `cell` while it still applies.
pub fn shape(stmt: &Statement<'_>, fresh: bool, cell: &ShapeCell) -> Arc<Shape> {
    let mut kept = cell.lock();
    match &*kept {
        Some(s) if !fresh && s.reprepared == stmt.get_status(StatementStatus::RePrepare) => {
            s.clone()
        }
        _ => {
            let s = Arc::new(Shape::of(stmt));
            *kept = Some(s.clone());
            s
        }
    }
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
