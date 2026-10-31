//! `VeltPgPool`: up to `max` connections, opened on demand and reused.
//!
//! A semaphore with `max` permits bounds the connections in use; idle connections wait in a
//! stack (the most recently used is reused first, so a quiet pool's extra connections are the
//! ones left idle), and a task takes one whose protocol task runs on its own worker first. An
//! operation takes a permit, an idle connection (or opens one), runs, and puts the connection
//! back; if the operation is cancelled, its connection is dropped instead.
//! Broken connections and ones left inside a transaction are never reused. `end` closes idle
//! connections and fails every later acquire with `ECLOSED`.

use super::client::{io_result, ClientHandle, ClientObj, Lease, CLIENTS};
use super::config::PgConfig;
use super::connection::{connect, Conn};
use super::error::{PgError, TLS};
use super::tls::PgTls;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use velt_native::handles::{Key, Registry};
use velt_native::{export, Error, Str};
use velt_text::db_json::parse_params;

/// A pool (see the module docs).
pub struct PoolObj {
    config: PgConfig,
    tls: PgTls,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<Arc<Conn>>>,
    ended: AtomicBool,
}

/// Opaque handle (a key into [`POOLS`]: the package's `Pool` is a Copy struct, see `client.rs`).
pub type PoolHandle = Key<PoolObj>;

static POOLS: Registry<PoolObj> = Registry::new();

impl PoolObj {
    /// Take a permit and a connection.
    async fn acquire(self: &Arc<Self>) -> Result<(Arc<Conn>, OwnedSemaphorePermit), PgError> {
        let closed = || PgError::closed("pool");
        if self.ended.load(Ordering::Acquire) {
            return Err(closed());
        }
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| closed())?;
        let here = velt_native::rt::worker_index();
        loop {
            let Some(conn) = self.take_idle(here) else {
                break;
            };
            if !conn.is_closed() {
                return Ok((conn, permit));
            }
        }
        // Opening one is boxed: inline, the connect future would make every pooled query's
        // future larger, copied on every query.
        let conn = Box::pin(connect(&self.config, self.tls.clone())).await?;
        Ok((Arc::new(conn), permit))
    }

    /// An idle connection: the most recently used one driven on worker `here`, else the most
    /// recently used.
    fn take_idle(&self, here: Option<usize>) -> Option<Arc<Conn>> {
        let mut idle = self.idle.lock();
        let at = here.and_then(|w| idle.iter().rposition(|c| c.driven_on(w)));
        match at {
            Some(i) => Some(idle.remove(i)),
            None => idle.pop(),
        }
    }

    /// Return a connection after use (dropped if the pool ended or it is not reusable).
    pub fn put_back(&self, conn: Arc<Conn>) {
        if !self.ended.load(Ordering::Acquire) && !conn.is_closed() && conn.depth() == 0 {
            self.idle.lock().push(conn);
        }
    }

    /// Run `op` on a pooled connection, then return it.
    async fn run<R, F, Fut>(self: Arc<Self>, op: F) -> Result<R, PgError>
    where
        F: FnOnce(Arc<Conn>) -> Fut,
        Fut: Future<Output = Result<R, PgError>>,
    {
        let (conn, permit) = self.acquire().await?;
        let r = op(conn.clone()).await;
        self.put_back(conn);
        drop(permit);
        r
    }
}

/// `createPool({ url, max })` → `IoResult<VeltPgPool>`. Parses the connection string (and
/// reads `sslrootcert`) now; connections open on first use. `max == 0` means 10.
#[export]
fn velt_postgres__pool_new(url: &str, max: u32) -> Result<u64, Error> {
    let r = super::config::parse(url).and_then(|config| {
        let tls = super::tls::connector(&config).map_err(|e| PgError::new(TLS, e))?;
        let max = if max == 0 { 10 } else { max as usize };
        Ok(POOLS
            .insert(PoolObj {
                config,
                tls,
                permits: Arc::new(Semaphore::new(max)),
                idle: Mutex::new(Vec::new()),
                ended: AtomicBool::new(false),
            })
            .bits())
    });
    io_result(r)
}

/// Run `op` on one of `pool`'s connections.
pub(super) async fn pool_op<R, F, Fut>(pool: u64, op: F) -> Result<R, Error>
where
    F: FnOnce(Arc<Conn>) -> Fut,
    Fut: Future<Output = Result<R, PgError>>,
{
    io_result(match POOLS.get(Key::from_bits(pool)) {
        Some(p) => p.run(op).await,
        None => Err(PgError::closed("pool")),
    })
}

/// `pool.connect()` → `IoResult<VeltPgClient>`: a dedicated connection until its `close()`.
#[export(async)]
async fn velt_postgres__pool_connect(pool: u64) -> Result<u64, Error> {
    let r = async {
        let pool = POOLS
            .get(Key::from_bits(pool))
            .ok_or_else(|| PgError::closed("pool"))?;
        let (conn, permit) = pool.acquire().await?;
        let lease = Lease { pool, permit };
        let client: ClientHandle = CLIENTS.insert(ClientObj::from_arc(conn, Some(lease)));
        Ok(client.bits())
    };
    io_result(r.await)
}

/// `pool.query(sql, params)` as a row cursor (as `velt_postgres__query_rows`).
#[export(async)]
async fn velt_postgres__pool_query_rows(
    pool: u64,
    sql: Str,
    params: Str,
    first_only: bool,
) -> Result<u64, Error> {
    pool_op(pool, move |c| async move {
        let parsed = parse_params(params.as_bytes()).map_err(PgError::invalid)?;
        let (statement, rows) = c.query_params(&sql.to_str(), parsed).await?;
        Ok(super::cursor::open(statement, rows, first_only))
    })
    .await
}

/// As `velt_postgres__query_bound`, on one of the pool's connections.
#[export(async)]
async fn velt_postgres__pool_query_bound(
    pool: u64,
    sql: Str,
    sink: u64,
    first_only: bool,
) -> Result<u64, Error> {
    let parsed = super::sink::take(sink).map_err(|e| e.to_error())?;
    pool_op(pool, move |c| async move {
        let (statement, rows) = c.query_params(&sql.to_str(), parsed).await?;
        Ok(super::cursor::open(statement, rows, first_only))
    })
    .await
}

/// As `velt_postgres__execute_bound`, on one of the pool's connections.
#[export(async)]
async fn velt_postgres__pool_execute_bound(pool: u64, sql: Str, sink: u64) -> Result<i64, Error> {
    let parsed = super::sink::take(sink).map_err(|e| e.to_error())?;
    pool_op(pool, move |c| async move {
        Ok(c.execute_params(&sql.to_str(), parsed).await? as i64)
    })
    .await
}

/// `pool.execute(sql, params)` → `IoResult<i64>` (as `velt_postgres__execute`).
#[export(async)]
async fn velt_postgres__pool_execute(pool: u64, sql: Str, params: Str) -> Result<i64, Error> {
    pool_op(pool, move |c| async move {
        Ok(c.execute(&sql.to_str(), params.as_bytes()).await? as i64)
    })
    .await
}

/// `pool.batch(sql)` → `IoStatus` (as `velt_postgres__batch`).
#[export(async)]
async fn velt_postgres__pool_batch(pool: u64, sql: Str) -> Result<(), Error> {
    pool_op(pool, move |c| async move { c.batch(&sql.to_str()).await }).await
}

/// Connections waiting idle in the pool.
#[export]
fn velt_postgres__pool_idle(pool: u64) -> u32 {
    POOLS
        .get(Key::from_bits(pool))
        .map_or(0, |p| p.idle.lock().len() as u32)
}

/// End the pool and release the handle (a closed or null handle ⇒ no-op): idle connections
/// close now, busy ones when their operation or client finishes; later operations fail with
/// `ECLOSED`.
#[export]
fn velt_postgres__pool_end(pool: u64) {
    if let Some(p) = POOLS.remove(Key::from_bits(pool)) {
        p.ended.store(true, Ordering::Release);
        p.permits.close();
        p.idle.lock().clear();
    }
}
