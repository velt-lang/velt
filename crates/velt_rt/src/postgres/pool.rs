//! `VeltPgPool`: up to `max` connections, opened on demand and reused.
//!
//! A semaphore with `max` permits bounds the connections in use; idle connections wait in a
//! stack (the most recently used is reused first, so a quiet pool's extra connections are the
//! ones left idle). An operation takes a permit, an idle connection (or opens one), runs, and
//! puts the connection back; if the operation is cancelled, its connection is dropped instead.
//! Broken connections and ones left inside a transaction are never reused. `end` closes idle
//! connections and fails every later acquire with `ECLOSED`.

use super::client::{bytes, io_result, text, ClientHandle, ClientObj, Lease, CLIENTS};
use super::config::PgConfig;
use super::connection::{connect, Conn};
use super::error::{PgError, TLS};
use super::tls::PgTls;
use crate::registry::{Key, Registry};
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// A pool (see the module docs).
pub struct PoolObj {
    config: PgConfig,
    tls: PgTls,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<Arc<Conn>>>,
    ended: AtomicBool,
}

/// Opaque handle (a key into [`POOLS`]: std's `Pool` is a Copy struct, see `client.rs`).
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
        loop {
            let Some(conn) = self.idle.lock().pop() else {
                break;
            };
            if !conn.is_closed() {
                return Ok((conn, permit));
            }
        }
        let conn = connect(&self.config, self.tls.clone()).await?;
        Ok((Arc::new(conn), permit))
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
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_new(
    url: *const VeltStr,
    max: u32,
    out: *mut IoResult<PoolHandle>,
) {
    let r = super::config::parse(&text(url)).and_then(|config| {
        let tls = super::tls::connector(&config).map_err(|e| PgError::new(TLS, e))?;
        let max = if max == 0 { 10 } else { max as usize };
        Ok(POOLS.insert(PoolObj {
            config,
            tls,
            permits: Arc::new(Semaphore::new(max)),
            idle: Mutex::new(Vec::new()),
            ended: AtomicBool::new(false),
        }))
    });
    out.write(io_result(r));
}

/// A leaf future running `op` on one of `pool`'s connections.
unsafe fn pool_op<R, F, Fut>(pool: PoolHandle, op: F) -> *mut VeltFut
where
    R: Send + 'static,
    F: FnOnce(Arc<Conn>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<R, PgError>> + Send,
{
    let obj = POOLS.get(pool);
    new_leaf(async move {
        io_result(match obj {
            Some(p) => p.run(op).await,
            None => Err(PgError::closed("pool")),
        })
    })
}

/// `pool.connect()` → `IoResult<VeltPgClient>`: a dedicated connection until its `close()`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_connect(pool: PoolHandle) -> *mut VeltFut {
    let obj = POOLS.get(pool);
    new_leaf(async move {
        let r = async {
            let pool = obj.ok_or_else(|| PgError::closed("pool"))?;
            let (conn, permit) = pool.acquire().await?;
            let lease = Lease { pool, permit };
            let client: ClientHandle = CLIENTS.insert(ClientObj::from_arc(conn, Some(lease)));
            Ok(client)
        };
        io_result(r.await)
    })
}

/// `pool.query(sql, params)` → `IoResult<string>` (as `velt_rt_pg_query`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_query(
    pool: PoolHandle,
    sql: *const VeltStr,
    params: *const VeltStr,
    first_only: u8,
) -> *mut VeltFut {
    let (sql, params) = (text(sql), bytes(params));
    pool_op(pool, move |c| async move {
        let json = c.query(&sql, &params, first_only != 0).await?;
        Ok(VeltStr::from_vec(json))
    })
}

/// `pool.execute(sql, params)` → `IoResult<i64>` (as `velt_rt_pg_execute`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_execute(
    pool: PoolHandle,
    sql: *const VeltStr,
    params: *const VeltStr,
) -> *mut VeltFut {
    let (sql, params) = (text(sql), bytes(params));
    pool_op(pool, move |c| async move {
        Ok(c.execute(&sql, &params).await? as i64)
    })
}

/// `pool.batch(sql)` → `VeltErr` (as `velt_rt_pg_batch`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_batch(
    pool: PoolHandle,
    sql: *const VeltStr,
) -> *mut VeltFut {
    let sql = text(sql);
    pool_op(pool, move |c| async move { c.batch(&sql).await })
}

/// Connections waiting idle in the pool.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_idle(pool: PoolHandle) -> u32 {
    POOLS.get(pool).map_or(0, |p| p.idle.lock().len() as u32)
}

/// End the pool and release the handle (a closed or null handle ⇒ no-op): idle connections
/// close now, busy ones when their operation or client finishes; later operations fail with
/// `ECLOSED`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_end(pool: PoolHandle) {
    if let Some(p) = POOLS.remove(pool) {
        p.ended.store(true, Ordering::Release);
        p.permits.close();
        p.idle.lock().clear();
    }
}
