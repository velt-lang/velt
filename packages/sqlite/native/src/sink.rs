//! A statement's parameter sink (`velt_text::rows_cursor::ParamSink`, native_abi.md "Parameter
//! sinks"): velt/db's `bindParams` (code generated per type) hands it the parameters value by value, and it binds each at
//! once. It binds as the JSON text of the same values would (bind.rs): an object by name (each
//! `:name`, `@name` or `$name` takes the member `name`; members no parameter names are
//! ignored), anything else by position; what is missing or extra fails with the same message
//! when the Velt side hands the sink on to run the statement.
//!
//! The sink holds the connection's lock from [`begin`] until the Velt side hands it on (to [`rows`] or
//! [`run`]) or gives it up ([`abort`]); velt/db's `bindParams` (code generated per type) runs in between, nothing else.

use super::connection::Locked;
use super::cursor;
use super::error::DbError;
use super::raw::{Bind, RawStmt};
use super::statement::{Prepared, RunInfo};
use std::sync::Arc;
use velt_text::rows_cursor::{close_sink, open_sink, Cell, ParamSink};

struct SqliteSink {
    prepared: Arc<Prepared>,
    /// An object's members by name, else values by position.
    named: bool,
    /// The values given by position so far.
    values: usize,
    /// The name of the member whose value comes next.
    name: Vec<u8>,
    /// Which parameters (from 1) an object's member bound.
    bound: Vec<bool>,
    failure: Option<DbError>,
    locked: Locked,
}

impl SqliteSink {
    fn stmt(&self) -> &RawStmt {
        &self.prepared.stmt
    }

    fn bind(&mut self, i: usize, v: Cell<'_>) {
        let v = match v {
            Cell::Null => Bind::Null,
            Cell::Bool(b) => Bind::Int(b as i64),
            Cell::Int(v) => Bind::Int(v),
            Cell::Float(v) => Bind::Float(v),
            // A nested value binds as its JSON text, as SQLite stores JSON.
            Cell::Text(t) | Cell::Ascii(t) | Cell::Json(t) => Bind::Text(t),
            Cell::Blob(b) => Bind::Blob(b),
        };
        if let Err(e) = self.stmt().bind(i, v) {
            self.failure.get_or_insert(e);
        }
    }

    /// The failure binding met, or what is missing or extra (bind.rs's messages, in its order).
    fn check(&mut self) -> Result<(), DbError> {
        if let Some(e) = self.failure.take() {
            return Err(e);
        }
        let count = self.stmt().parameter_count();
        if !self.named {
            // Values were given (an empty array is no values, not none).
            return match self.values {
                n if n == count => Ok(()),
                n => Err(DbError::range(format!(
                    "the statement has {count} parameter(s) but {n} value(s) were given"
                ))),
            };
        }
        for i in 1..=count {
            let key = match self.stmt().parameter_name(i) {
                Some(name) if !name.starts_with('?') => name[1..].to_string(),
                _ => {
                    return Err(DbError::range(format!(
                        "parameter {i} is positional (`?`); pass an array to bind it"
                    )))
                }
            };
            if !self.bound.get(i).copied().unwrap_or(false) {
                return Err(DbError::range(format!("missing named parameter \"{key}\"")));
            }
        }
        Ok(())
    }
}

impl ParamSink for SqliteSink {
    fn object(&mut self) {
        self.named = true;
    }

    fn name(&mut self, name: &[u8]) {
        self.name.clear();
        self.name.extend_from_slice(name);
    }

    fn value(&mut self, v: Cell<'_>) {
        if !self.named {
            self.values += 1;
            if self.values <= self.stmt().parameter_count() {
                self.bind(self.values, v);
            }
            return;
        }
        // Every parameter this member names (`:id` and `$id` alike).
        let count = self.stmt().parameter_count();
        for i in 1..=count {
            let names = self
                .stmt()
                .parameter_name(i)
                .is_some_and(|n| !n.starts_with('?') && n.as_bytes()[1..] == self.name[..]);
            if names {
                if self.bound.len() <= count {
                    self.bound.resize(count + 1, false);
                }
                self.bound[i] = true;
                self.bind(i, v);
            }
        }
    }
}

/// A sink for `prepared`'s next run on the locked connection: its handle for the Velt side.
pub(crate) fn begin(mut locked: Locked, prepared: &Arc<Prepared>) -> Result<u64, DbError> {
    locked.conn()?;
    prepared.stmt.finish();
    // Registered with the runtime: the Velt side holds a handle the runtime checks.
    Ok(open_sink(Box::new(SqliteSink {
        prepared: prepared.clone(),
        named: false,
        values: 0,
        name: Vec::new(),
        bound: Vec::new(),
        failure: None,
        locked,
    })))
}

/// The sink of `handle`, taken back from the Velt side (`SQLITE_MISUSE` when it names no open
/// sink: given on already, made up).
fn taken(handle: u64) -> Result<Box<SqliteSink>, DbError> {
    close_sink::<SqliteSink>(handle).ok_or_else(|| DbError::closed("parameter sink"))
}

/// The sink of `handle`, taken back from the Velt side, with its bindings checked.
fn take(handle: u64) -> Result<Box<SqliteSink>, DbError> {
    let mut sink = taken(handle)?;
    match sink.check() {
        Ok(()) => Ok(sink),
        Err(e) => {
            sink.stmt().finish();
            sink.locked.note(e.clone());
            Err(e)
        }
    }
}

/// Steps the bound statement to its first row: a cursor over its rows (`cursor::open`).
pub fn rows(handle: u64, single: bool) -> Result<u64, DbError> {
    let sink = *take(handle)?;
    cursor::open(sink.locked, &sink.prepared, single, |_| Ok(()))
}

/// Runs the bound statement to completion.
pub fn run(handle: u64) -> Result<RunInfo, DbError> {
    let mut sink = take(handle)?;
    match super::rows::drain(sink.stmt()) {
        Ok(()) => {
            let conn = sink.locked.conn()?;
            Ok(RunInfo {
                changes: conn.changes() as i64,
                last_insert_rowid: conn.last_insert_rowid(),
            })
        }
        Err(e) => {
            sink.locked.note(e.clone());
            Err(e)
        }
    }
}

/// Gives a sink up unused (the Velt side binds the parameters as JSON text instead); nothing for
/// a handle that names no open sink.
pub fn abort(handle: u64) {
    if let Ok(sink) = taken(handle) {
        sink.stmt().finish();
    }
}
