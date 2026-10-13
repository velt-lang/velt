//! Reading an executing SQLite statement's rows for std's `JSON.parse<T[]>`, in the binary
//! form the JSON reader walks directly ([`crate::json::rows_bin`]).
//!
//! SQLite has no boolean type, so booleans come back as 0/1. A column whose declared type is
//! `BOOL` or `BOOLEAN` (a table column read directly, not an expression) is written as
//! `true`/`false` when it holds 0 or 1, so it decodes into a `bool` field; any other column
//! stays a number.

use super::error::DbError;
use crate::json::rows_bin::BinRows;
use rusqlite::types::ValueRef;
use rusqlite::Statement;

/// Step `stmt` (already bound) and encode its rows: every row, or with `first_only` the first
/// row alone (nothing at all if there is none; the statement is not stepped further).
pub fn read(stmt: &mut Statement<'_>, first_only: bool) -> Result<Vec<u8>, DbError> {
    let count = stmt.column_count();
    // A name SQLite cannot give (out of memory) is empty.
    let names = (0..count).map(|i| stmt.column_name(i).unwrap_or(""));
    let mut w = BinRows::new(names, first_only);
    // Which columns are declared boolean: looked up when the first 0 or 1 turns up, since most
    // results have none and the declared types cost a call per column.
    let mut bools: Option<Vec<bool>> = None;
    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        for i in 0..count {
            match row.get_ref(i)? {
                ValueRef::Null => w.null(),
                ValueRef::Integer(v @ (0 | 1)) => {
                    let bools = bools.get_or_insert_with(|| declared_bools(row.as_ref()));
                    if bools[i] {
                        w.bool(v == 1);
                    } else {
                        w.int(v);
                    }
                }
                ValueRef::Integer(v) => w.int(v),
                ValueRef::Real(v) => w.float(v),
                ValueRef::Text(t) => w.text(t),
                ValueRef::Blob(b) => w.bytes(b),
            }
        }
        w.end_row();
        if first_only {
            return Ok(w.finish());
        }
    }
    Ok(if first_only { Vec::new() } else { w.finish() })
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

/// Step `stmt` (already bound) to completion, discarding any rows.
pub fn drain(stmt: &mut Statement<'_>) -> Result<(), DbError> {
    let mut rows = stmt.raw_query();
    while rows.next()?.is_some() {}
    Ok(())
}
