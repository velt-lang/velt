//! Result rows as JSON text (`[{"col":value,...}]`, `crate::db_json::RowWriter`), each value
//! converted by [`super::types`]. Rows are read as raw bytes (a pass-through `FromSql`), so no
//! intermediate Rust values are built.

use super::error::{PgError, UNSUPPORTED};
use super::types::{decodable, push_value};
use crate::db_json::RowWriter;
use std::error::Error as StdError;
use tokio_postgres::types::{FromSql, Type};
use tokio_postgres::{Column, Row};

/// A column's raw binary value (`None` = NULL).
struct Raw<'a>(Option<&'a [u8]>);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(Some(raw)))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(None))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn unsupported(column: &Column, what: &str) -> PgError {
    PgError::new(
        UNSUPPORTED,
        format!(
            "column \"{}\" has {what}, which std/postgres cannot decode; cast it, e.g. \"{}\"::text",
            column.name(),
            column.name()
        ),
    )
}

/// Fails for a column whose type cannot be decoded.
pub fn check_columns(columns: &[Column]) -> Result<(), PgError> {
    match columns.iter().find(|c| !decodable(c.type_())) {
        Some(c) => Err(unsupported(c, &format!("type {}", c.type_().name()))),
        None => Ok(()),
    }
}

/// `rows` as a JSON array, or with `first_only` the first row's object (empty if none).
pub fn to_json(columns: &[Column], rows: &[Row], first_only: bool) -> Result<Vec<u8>, PgError> {
    let mut w = RowWriter::new(columns.iter().map(Column::name));
    let take = if first_only { 1 } else { rows.len() };
    for row in rows.iter().take(take) {
        w.begin_row();
        for (i, column) in columns.iter().enumerate() {
            let raw = row.try_get::<_, Raw>(i).map_err(PgError::from)?;
            match raw.0 {
                None => w.null(),
                Some(bytes) => push_value(w.raw_value(), column.type_(), bytes)
                    .map_err(|e| unsupported(column, &e))?,
            }
        }
        w.end_row();
    }
    Ok(if first_only {
        w.into_rows()
    } else {
        w.into_array()
    })
}
