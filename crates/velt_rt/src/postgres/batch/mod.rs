//! Batches: one statement run with N parameter sets in a single message group — `Bind` +
//! `Execute` per set and **one** `Sync` — sent through the connection's wire (`super::wire`).
//! This is what pgx's `Batch` does: one round trip, one implicit transaction and one flush on
//! the server, where N pipelined tokio-postgres queries cost N of each.
//!
//! The statement is prepared by tokio-postgres as for `query` (the cache gives its parameter
//! and column types) and a second time under the batch's own name (`velt_b<n>`, a `Parse` at
//! the head of the first group that uses it), because tokio-postgres keeps its statement names
//! private. That twin is closed on the server when the cache evicts the statement.
//!
//! Error semantics are the server's: when an execution fails, the server skips the rest of the
//! group up to the `Sync`, so the whole batch fails with that execution's error (its SQLSTATE),
//! and nothing after it ran. Outside a transaction the group is one implicit transaction, so
//! the writes before the failure are rolled back too: a batch is all or nothing. Inside a
//! transaction (`begin`), the failure aborts that transaction, like any failed statement.

mod abi;
mod decode;
mod encode;
#[cfg(test)]
mod tests;

use super::bind::bind_all;
use super::connection::Conn;
use super::error::PgError;
use super::rows;
use crate::db_json::{parse_param_list, Params};
use bytes::BytesMut;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// What a batch returns for each execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every row (`[[{...},...],...]`).
    Rows,
    /// The first row's object or `null` (`[{...},null,...]`).
    FirstRow,
    /// The rows affected (`[1,0,...]`).
    Count,
}

impl Mode {
    /// The ABI's `mode` byte (rt_abi_async.md §14.18); unknown values read as `Rows`.
    pub fn from_abi(mode: u8) -> Mode {
        match mode {
            1 => Mode::FirstRow,
            2 => Mode::Count,
            _ => Mode::Rows,
        }
    }
}

/// A connection's batch bookkeeping.
#[derive(Default)]
pub struct BatchState {
    /// Orders "decide whether to `Parse`" with "queue the group", so a group that relies on
    /// another's `Parse` is never written before it.
    order: Mutex<()>,
    next_id: AtomicU64,
    /// Names of evicted batch statements, closed by the next group.
    closing: Arc<Mutex<Vec<String>>>,
}

/// The batch twin of a cached statement has not been sent.
const UNSENT: u8 = 0;
/// It exists on the server.
const READY: u8 = 1;
/// It may or may not exist (a group with its `Parse` failed, or its plan went stale): the next
/// group closes and parses it again.
const DOUBTFUL: u8 = 2;

/// A cached statement's batch twin (see the module docs).
pub struct BatchStatement {
    name: String,
    /// The SQL as prepared (named placeholders already rewritten to `$n`).
    sql: String,
    state: AtomicU8,
    closing: Arc<Mutex<Vec<String>>>,
}

impl BatchStatement {
    /// The twin for `sql` on the connection with `batches` (nothing is sent yet).
    pub fn new(sql: String, batches: &BatchState) -> Arc<BatchStatement> {
        let id = batches.next_id.fetch_add(1, Ordering::Relaxed);
        Arc::new(BatchStatement {
            name: format!("velt_b{id}"),
            sql,
            state: AtomicU8::new(UNSENT),
            closing: batches.closing.clone(),
        })
    }
}

impl Drop for BatchStatement {
    fn drop(&mut self) {
        if *self.state.get_mut() != UNSENT {
            self.closing.lock().push(std::mem::take(&mut self.name));
        }
    }
}

/// SQLSTATEs after which the twin is prepared again (as `connection::STALE_PLAN`).
const STALE_PLAN: [&str; 2] = ["0A000", "26000"];

impl Conn {
    /// Run `sql` once per parameter set in `sets` (a JSON array of sets) as one group; the
    /// results as JSON per `mode`.
    pub async fn run_batch(&self, sql: &str, sets: &[u8], mode: Mode) -> Result<Vec<u8>, PgError> {
        let sets = parse_param_list(sets).map_err(PgError::invalid)?;
        let Some(first) = sets.first() else {
            return Ok(b"[]".to_vec());
        };
        let named = matches!(first, Params::Named(_));
        let r = self.batch_of(sql, named, &sets, mode).await;
        self.forget_if_stale(sql, named, &r);
        r
    }

    async fn batch_of(
        &self,
        sql: &str,
        named: bool,
        sets: &[Params<'_>],
        mode: Mode,
    ) -> Result<Vec<u8>, PgError> {
        let p = self.prepared(sql, named).await?;
        let (types, columns) = (p.statement.params(), p.statement.columns());
        if mode != Mode::Count {
            rows::check_columns(columns)?;
        }
        let mut body = BytesMut::new();
        for set in sets {
            let values = bind_all(set, p.names.as_deref(), types)?;
            encode::execution(&p.batch.name, &values, &mut body)?;
        }
        encode::sync(&mut body);
        // Held until the replies are in: no COPY FROM STDIN starts around this group.
        let _gate = self.copy_gate.read().await;
        let (replies, parsing) = {
            let _order = self.batches.order.lock();
            let mut group = BytesMut::new();
            for name in std::mem::take(&mut *self.batches.closing.lock()) {
                encode::close(&name, &mut group)?;
            }
            let before = p.batch.state.swap(READY, Ordering::AcqRel);
            if before == DOUBTFUL {
                encode::close(&p.batch.name, &mut group)?;
            }
            let parsing = before != READY;
            if parsing {
                encode::parse(&p.batch.name, &p.batch.sql, types, &mut group)?;
            }
            group.extend_from_slice(&body);
            (self.wire.send(group.freeze())?, parsing)
        };
        let replies = replies.await.map_err(|_| super::wire::closed())?;
        let r = decode::results(replies, columns, sets.len(), mode);
        if let Err(e) = &r {
            if parsing || STALE_PLAN.contains(&e.code.as_str()) {
                p.batch.state.store(DOUBTFUL, Ordering::Release);
            }
        }
        r
    }
}
