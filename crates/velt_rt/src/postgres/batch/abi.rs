//! The batch ABI (rt_abi_async.md §14.18): `client.batchQuery` & co. and their pool forms
//! (a pool runs the whole batch on one of its connections).

use super::Mode;
use crate::postgres::client::{bytes, client_op, text, ClientHandle};
use crate::postgres::pool::{pool_op, PoolHandle};
use crate::str::VeltStr;
use crate::task::VeltFut;

/// `client.batchQuery(sql, paramSets)` & co. → `IoResult<string>`: `sql` run once per
/// parameter set of `sets` (a JSON array of sets) in one group with one `Sync`; per execution
/// its rows (`mode` 0), its first row or `null` (1), or its rows affected (2), as a JSON array.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_query_batch(
    client: ClientHandle,
    sql: *const VeltStr,
    sets: *const VeltStr,
    mode: u8,
) -> *mut VeltFut {
    let (sql, sets, mode) = (text(sql), bytes(sets), Mode::from_abi(mode));
    client_op(client, move |c| async move {
        Ok(VeltStr::from_vec(c.run_batch(&sql, &sets, mode).await?))
    })
}

/// `pool.batchQuery(sql, paramSets)` & co. (as `velt_rt_pg_query_batch`, on one pooled
/// connection).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_pool_query_batch(
    pool: PoolHandle,
    sql: *const VeltStr,
    sets: *const VeltStr,
    mode: u8,
) -> *mut VeltFut {
    let (sql, sets, mode) = (text(sql), bytes(sets), Mode::from_abi(mode));
    pool_op(pool, move |c| async move {
        Ok(VeltStr::from_vec(c.run_batch(&sql, &sets, mode).await?))
    })
}
