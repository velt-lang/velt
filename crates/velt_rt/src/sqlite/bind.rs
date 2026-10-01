//! Binding parsed JSON parameters ([`Params`]) to an SQLite statement.
//!
//! - An object binds by name: each `:name`, `@name` or `$name` in the SQL takes the member
//!   `name`. A parameter without a member is an error (like better-sqlite3); extra members are
//!   ignored, so one params object can serve several statements.
//! - An array (or a lone scalar) binds by position and must have exactly one value per
//!   parameter (`?`, `?NNN`, or named ones in order).
//! - No params (`run()`, `all()` …) is only valid for a statement without parameters.
//!
//! Values: int → INTEGER, float → REAL, string → TEXT, bool → INTEGER 0/1, null → NULL,
//! byte array → BLOB. Text and blobs are bound straight from the parsed JSON (SQLite copies
//! them), so binding allocates nothing.

use super::error::DbError;
use crate::db_json::{DbValue, Params};
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::Statement;

/// Bind `params` to every parameter of `stmt` (see the module docs).
pub fn bind(stmt: &mut Statement<'_>, params: &Params<'_>) -> Result<(), DbError> {
    let count = stmt.parameter_count();
    match params {
        Params::None if count == 0 => Ok(()),
        Params::None => Err(DbError::range(format!(
            "the statement has {count} parameter(s) but none were given (use runWith / getWith / allWith)"
        ))),
        Params::Positional(values) if values.len() != count => Err(DbError::range(format!(
            "the statement has {count} parameter(s) but {} value(s) were given",
            values.len()
        ))),
        Params::Positional(values) => {
            for (i, v) in values.iter().enumerate() {
                bind_value(stmt, i + 1, v)?;
            }
            Ok(())
        }
        Params::Named(_) => bind_named(stmt, params, count),
    }
}

fn bind_named(stmt: &mut Statement<'_>, params: &Params<'_>, count: usize) -> Result<(), DbError> {
    for i in 1..=count {
        let key = match stmt.parameter_name(i) {
            Some(name) if !name.starts_with('?') => &name[1..],
            _ => {
                return Err(DbError::range(format!(
                    "parameter {i} is positional (`?`); pass an array to bind it"
                )))
            }
        };
        let Some(value) = params.named(key) else {
            return Err(DbError::range(format!("missing named parameter \"{key}\"")));
        };
        bind_value(stmt, i, value)?;
    }
    Ok(())
}

fn bind_value(stmt: &mut Statement<'_>, index: usize, v: &DbValue<'_>) -> Result<(), DbError> {
    let value = match v {
        DbValue::Null => ValueRef::Null,
        DbValue::Bool(b) => ValueRef::Integer(*b as i64),
        DbValue::Int(i) => ValueRef::Integer(*i),
        DbValue::Float(f) => ValueRef::Real(*f),
        DbValue::Text(s) => ValueRef::Text(s.as_bytes()),
        DbValue::Bytes(b) => ValueRef::Blob(b),
    };
    stmt.raw_bind_parameter(index, ToSqlOutput::Borrowed(value))?;
    Ok(())
}
