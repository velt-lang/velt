//! A batch's replies (every server message up to its `ReadyForQuery`) as one JSON array with
//! an entry per execution (see [`Mode`]), or the first error the server reported.

use super::Mode;
use crate::db_json::RowWriter;
use crate::postgres::error::PgError;
use crate::postgres::rows::push_row;
use bytes::BytesMut;
use postgres_protocol::message::backend::{
    CommandCompleteBody, DataRowBody, ErrorResponseBody, Message,
};
use tokio_postgres::fallible_iterator::FallibleIterator;
use tokio_postgres::Column;

fn protocol(what: impl std::fmt::Display) -> PgError {
    PgError::new("UNKNOWN", format!("unexpected batch reply: {what}"))
}

/// The JSON of `executions` results from `replies` (see the module docs).
pub fn results(
    mut replies: BytesMut,
    columns: &[Column],
    executions: usize,
    mode: Mode,
) -> Result<Vec<u8>, PgError> {
    let mut out = vec![b'['];
    let mut done = 0;
    let mut set: Option<RowWriter> = None;
    let mut error = None;
    while let Some(message) = Message::parse(&mut replies).map_err(protocol)? {
        match message {
            Message::ParseComplete | Message::CloseComplete => {}
            Message::BindComplete => {
                set = Some(RowWriter::new(columns.iter().map(Column::name)));
            }
            Message::DataRow(row) => {
                let w = set
                    .as_mut()
                    .ok_or_else(|| protocol("a row before BindComplete"))?;
                if mode == Mode::Rows || (mode == Mode::FirstRow && w.rows() == 0) {
                    push_data_row(w, columns, &row)?;
                }
            }
            Message::CommandComplete(_) | Message::EmptyQueryResponse => {
                let w = set
                    .take()
                    .ok_or_else(|| protocol("a result before BindComplete"))?;
                if done > 0 {
                    out.push(b',');
                }
                finish_set(&mut out, w, &message, mode)?;
                done += 1;
            }
            Message::ErrorResponse(body) => {
                // The server skips to the Sync after an error: there is only one.
                error.get_or_insert(db_error(&body));
            }
            Message::ReadyForQuery(_) => break,
            _ => return Err(protocol("an unknown message")),
        }
    }
    if let Some(e) = error {
        return Err(e);
    }
    if done != executions {
        return Err(protocol(format!(
            "{done} results for {executions} executions"
        )));
    }
    out.push(b']');
    Ok(out)
}

/// Append one execution's entry.
fn finish_set(out: &mut Vec<u8>, w: RowWriter, end: &Message, mode: Mode) -> Result<(), PgError> {
    match mode {
        Mode::Rows => out.extend_from_slice(&w.into_array()),
        Mode::FirstRow if w.rows() == 0 => out.extend_from_slice(b"null"),
        Mode::FirstRow => out.extend_from_slice(&w.into_rows()),
        Mode::Count => {
            let n = match end {
                Message::CommandComplete(body) => rows_affected(body)?,
                _ => 0,
            };
            out.extend_from_slice(n.to_string().as_bytes());
        }
    }
    Ok(())
}

fn push_data_row(w: &mut RowWriter, columns: &[Column], row: &DataRowBody) -> Result<(), PgError> {
    let buffer = row.buffer();
    let ranges: Vec<_> = row.ranges().collect().map_err(protocol)?;
    if ranges.len() != columns.len() {
        return Err(protocol("a row with the wrong number of columns"));
    }
    let values = ranges.into_iter().map(|r| Ok(r.map(|r| &buffer[r])));
    push_row(w, columns, values)
}

/// The count at the end of a command tag (`UPDATE 3`, `INSERT 0 1`, `SELECT 5`); 0 if none.
fn rows_affected(body: &CommandCompleteBody) -> Result<u64, PgError> {
    let tag = body.tag().map_err(protocol)?;
    Ok(tag
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0))
}

/// The server's error: SQLSTATE, message, detail and constraint.
fn db_error(body: &ErrorResponseBody) -> PgError {
    let mut e = PgError::new("UNKNOWN", "");
    let mut fields = body.fields();
    while let Ok(Some(f)) = fields.next() {
        let value = String::from_utf8_lossy(f.value_bytes()).into_owned();
        match f.type_() {
            b'C' => e.code = value,
            b'M' => e.message = value,
            b'D' => e.detail = Some(value),
            b'n' => e.constraint = Some(value),
            _ => {}
        }
    }
    e
}
