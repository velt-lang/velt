//! The batch ABI (rt_abi_async.md §14.18): `client.batchQuery` & co. and their pool forms
//! (a pool runs the whole batch on one of its connections).

use super::Mode;
use crate::client::client_op;
use crate::pool::pool_op;
use velt_native::{export, Error, Str, StrBuf};

/// `client.batchQuery(sql, paramSets)` & co. → `IoResult<string>`: `sql` run once per
/// parameter set of `sets` (a JSON array of sets) in one group with one `Sync`; per execution
/// its rows (`mode` 0), its first row or `null` (1), or its rows affected (2), as a JSON array.
#[export(async)]
async fn velt_std_postgres__query_batch(
    client: u64,
    sql: Str,
    sets: Str,
    mode: u8,
) -> Result<Str, Error> {
    let mode = Mode::from_abi(mode);
    client_op(client, move |c| async move {
        let out = StrBuf::new();
        Ok(c.run_batch(&sql.to_str(), sets.as_bytes(), mode, out)
            .await?
            .finish())
    })
    .await
}

/// `pool.batchQuery(sql, paramSets)` & co. (as `velt_std_postgres__query_batch`, on one pooled
/// connection).
#[export(async)]
async fn velt_std_postgres__pool_query_batch(
    pool: u64,
    sql: Str,
    sets: Str,
    mode: u8,
) -> Result<Str, Error> {
    let mode = Mode::from_abi(mode);
    pool_op(pool, move |c| async move {
        let out = StrBuf::new();
        Ok(c.run_batch(&sql.to_str(), sets.as_bytes(), mode, out)
            .await?
            .finish())
    })
    .await
}
