//! One PostgreSQL connection: a tokio-postgres `Client` (its protocol task runs on the runtime),
//! the connection's wire (for batches, `super::wire`), the prepared-statement cache and the
//! transaction depth. Used by standalone clients and by the pool alike; every method takes
//! `&self`, so tasks may share a connection (tokio-postgres pipelines their queries).

use super::batch::{BatchState, BatchStatement};
use super::bind::{bind_all, PgParam};
use super::config::PgConfig;
use super::error::PgError;
use super::placeholders::rewrite_named;
use super::rows;
use super::statements::{Prepared, StatementCache};
use super::tls::PgTls;
use super::wire::Wire;
use crate::db_json::{parse_params, Params};
use bytes::Bytes;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, CopyInSink, CopyOutStream};

/// SQLSTATEs after which a cached statement is re-prepared: its plan no longer matches the
/// schema (`0A000` "cached plan must not change result type"), or it is gone (`26000`).
const STALE_PLAN: [&str; 2] = ["0A000", "26000"];

/// An open connection.
pub struct Conn {
    client: Client,
    cache: Mutex<StatementCache>,
    /// Open transaction levels (0 = none, 1 = BEGIN, n = savepoint `velt_tx_n`).
    depth: AtomicU32,
    /// Where batches send their message groups.
    pub(super) wire: Wire,
    /// Batch statement names and the lock that orders their preparation.
    pub(super) batches: BatchState,
    /// Held (shared) by batches in flight, exclusively while a `COPY … FROM STDIN` starts: a
    /// batch written between the copy's request and the server's `CopyInResponse` would land
    /// inside the copy (later, the wire itself holds batches back until the copy ends).
    pub(super) copy_gate: tokio::sync::RwLock<()>,
}

/// Connect with `config` (TLS per its `sslmode`).
pub async fn connect(config: &PgConfig, tls: PgTls) -> Result<Conn, PgError> {
    let (client, connection, wire) = super::socket::connect(&config.inner, &tls).await?;
    crate::task::runtime::handle().spawn(async move {
        // The task ends when the client is dropped or the server goes away; errors surface
        // on the client's next call ("connection closed").
        let _ = connection.await;
    });
    Ok(Conn {
        client,
        cache: Mutex::new(StatementCache::default()),
        depth: AtomicU32::new(0),
        wire,
        batches: BatchState::default(),
        copy_gate: tokio::sync::RwLock::new(()),
    })
}

/// Whether `params` (JSON) binds by name (an object).
fn is_named(params: &[u8]) -> bool {
    params.trim_ascii_start().first() == Some(&b'{')
}

/// The SQL text a statement is cached under (surrounding whitespace does not matter).
fn cache_key(sql: &str) -> &str {
    sql.trim()
}

impl Conn {
    /// Whether the connection is unusable (server gone, protocol error).
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Open transaction levels.
    pub fn depth(&self) -> u32 {
        self.depth.load(Ordering::Acquire)
    }

    /// The cached statement for `sql`, prepared on a miss.
    pub(super) async fn prepared(&self, sql: &str, named: bool) -> Result<Prepared, PgError> {
        let key = cache_key(sql);
        if let Some(p) = self.cache.lock().get(key, named) {
            return Ok(p);
        }
        let (text, names) = if named {
            let r = rewrite_named(key).map_err(PgError::invalid)?;
            (r.sql, Some(Arc::from(r.names)))
        } else {
            (key.to_string(), None)
        };
        let statement = self.client.prepare(&text).await?;
        let batch = BatchStatement::new(text, &self.batches);
        let p = Prepared {
            statement,
            names,
            batch,
        };
        self.cache.lock().insert(key, named, p.clone());
        Ok(p)
    }

    /// Prepare `sql` and bind `params` (JSON) to it.
    async fn bound(&self, sql: &str, params: &[u8]) -> Result<(Prepared, Vec<PgParam>), PgError> {
        let parsed = parse_params(params).map_err(PgError::invalid)?;
        let named = matches!(parsed, Params::Named(_));
        let p = self.prepared(sql, named).await?;
        let values = bind_all(&parsed, p.names.as_deref(), p.statement.params())?;
        Ok((p, values))
    }

    /// Drop `sql` (prepared for named parameters or not) from the cache when `r` says its
    /// plan is stale.
    pub(super) fn forget_if_stale<T>(&self, sql: &str, named: bool, r: &Result<T, PgError>) {
        if let Err(e) = r {
            if STALE_PLAN.contains(&e.code.as_str()) {
                self.cache.lock().remove(cache_key(sql), named);
            }
        }
    }

    /// Run `sql` with `params`; the rows as JSON (see [`rows::to_json`]).
    pub async fn query(
        &self,
        sql: &str,
        params: &[u8],
        first_only: bool,
    ) -> Result<Vec<u8>, PgError> {
        let r = async {
            let (p, values) = self.bound(sql, params).await?;
            rows::check_columns(p.statement.columns())?;
            let refs: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v as _).collect();
            let rows = self.client.query(&p.statement, &refs).await?;
            rows::to_json(p.statement.columns(), &rows, first_only)
        }
        .await;
        self.forget_if_stale(sql, is_named(params), &r);
        r
    }

    /// Run `sql` with `params`; the number of rows it affected (or returned).
    pub async fn execute(&self, sql: &str, params: &[u8]) -> Result<u64, PgError> {
        let r = async {
            let (p, values) = self.bound(sql, params).await?;
            let refs: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v as _).collect();
            Ok(self.client.execute(&p.statement, &refs).await?)
        }
        .await;
        self.forget_if_stale(sql, is_named(params), &r);
        r
    }

    /// Run `;`-separated statements without parameters (simple protocol, nothing cached).
    pub async fn batch(&self, sql: &str) -> Result<(), PgError> {
        Ok(self.client.batch_execute(sql).await?)
    }

    /// Start `COPY … FROM STDIN` (`sql` is not cached).
    pub async fn copy_in(&self, sql: &str) -> Result<CopyInSink<Bytes>, PgError> {
        let _no_batches = self.copy_gate.write().await;
        Ok(self.client.copy_in(sql).await?)
    }

    /// Start `COPY … TO STDOUT` (`sql` is not cached).
    pub async fn copy_out(&self, sql: &str) -> Result<CopyOutStream, PgError> {
        Ok(self.client.copy_out(sql).await?)
    }

    /// Open a transaction (a savepoint inside one); its level.
    pub async fn begin(&self) -> Result<u32, PgError> {
        let depth = self.depth() + 1;
        let sql = if depth == 1 {
            "BEGIN".to_string()
        } else {
            format!("SAVEPOINT velt_tx_{depth}")
        };
        self.client.batch_execute(&sql).await?;
        self.depth.store(depth, Ordering::Release);
        Ok(depth)
    }

    /// Commit or roll back transaction level `depth`, which must be the innermost one.
    pub async fn end(&self, depth: u32, commit: bool) -> Result<(), PgError> {
        let current = self.depth();
        if depth == 0 || depth != current {
            return Err(PgError::invalid(if depth > current {
                "the transaction has already ended".to_string()
            } else {
                "an inner transaction is still open; end it first".to_string()
            }));
        }
        let sql = match (depth, commit) {
            (1, true) => "COMMIT".to_string(),
            (1, false) => "ROLLBACK".to_string(),
            (d, true) => format!("RELEASE SAVEPOINT velt_tx_{d}"),
            (d, false) => {
                format!("ROLLBACK TO SAVEPOINT velt_tx_{d}; RELEASE SAVEPOINT velt_tx_{d}")
            }
        };
        // Whatever the outcome, this level is over: a failed COMMIT has rolled back, and a
        // failed savepoint command leaves the enclosing transaction aborted.
        let r = self.client.batch_execute(&sql).await;
        self.depth.store(depth - 1, Ordering::Release);
        Ok(r?)
    }
}
