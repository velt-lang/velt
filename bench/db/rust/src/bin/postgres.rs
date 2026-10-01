//! PostgreSQL workloads with tokio-postgres (bench/db/README.md) against $BENCH_PG_URL: a
//! per-run table, dropped at the end. Sequential workloads use one client with prepared
//! statements; the pool workloads use a deadpool-postgres pool of 8 (`prepare_cached` per
//! connection). Same SQL and sizes as bench/db/node/postgres.mjs.

use db_bench::{report, scale, unique_suffix, Row};
use deadpool_postgres::{Manager, Pool};
use std::error::Error;
use std::time::Instant;
use tokio_postgres::{Client, NoTls};

type BenchResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const POOL_SIZE: usize = 8;

fn to_row(r: &tokio_postgres::Row) -> Row {
    Row {
        id: i64::from(r.get::<_, i32>(0)),
        name: r.get(1),
        score: r.get(2),
    }
}

fn point_id(i: i64, rows: i64) -> i32 {
    ((i * 7919) % rows + 1) as i32
}

async fn insert_tx(c: &mut Client, table: &str, n: i64) -> BenchResult<()> {
    let sql = format!("INSERT INTO {table} (id, name, score) VALUES ($1, $2, $3)");
    let t0 = Instant::now();
    let tx = c.transaction().await?;
    let ins = tx.prepare(&sql).await?;
    let mut changes = 0;
    for i in 1..=n {
        let name = format!("name-{i}");
        changes += tx
            .execute(&ins, &[&(i as i32), &name, &(i as f64 + 0.5)])
            .await? as i64;
    }
    tx.commit().await?;
    report("postgres.insert_tx", n, changes, t0);
    Ok(())
}

async fn point_select(c: &Client, table: &str, n: i64, rows: i64) -> BenchResult<()> {
    let sel = c
        .prepare(&format!(
            "SELECT id, name, score FROM {table} WHERE id = $1"
        ))
        .await?;
    let t0 = Instant::now();
    let mut sum = 0;
    for i in 0..n {
        if let Some(r) = c.query_opt(&sel, &[&point_id(i, rows)]).await? {
            sum += to_row(&r).checksum();
        }
    }
    report("postgres.point_select", n, sum, t0);
    Ok(())
}

async fn range_select(c: &Client, table: &str, m: i64, rows: i64) -> BenchResult<()> {
    let sql = format!("SELECT id, name, score FROM {table} WHERE id >= $1 AND id < $2");
    let sel = c.prepare(&sql).await?;
    let t0 = Instant::now();
    let mut sum = 0;
    for j in 0..m {
        let lo = ((j * 7919) % (rows - 100) + 1) as i32;
        let found: Vec<Row> = c
            .query(&sel, &[&lo, &(lo + 100)])
            .await?
            .iter()
            .map(to_row)
            .collect();
        sum += found.iter().map(Row::checksum).sum::<i64>();
    }
    report("postgres.range_select", m, sum, t0);
    Ok(())
}

/// One task's share of a pool workload: queries t, t + tasks, t + 2 * tasks, ...
async fn pool_worker(
    pool: Pool,
    sql: String,
    t: i64,
    tasks: i64,
    n: i64,
    rows: i64,
) -> BenchResult<i64> {
    let mut sum = 0;
    for i in (t..n).step_by(tasks as usize) {
        let c = pool.get().await?;
        let sel = c.prepare_cached(&sql).await?;
        if let Some(r) = c.query_opt(&sel, &[&point_id(i, rows)]).await? {
            sum += to_row(&r).checksum();
        }
    }
    Ok(sum)
}

async fn pool_select(pool: &Pool, table: &str, tasks: i64, n: i64, rows: i64) -> BenchResult<()> {
    let sql = format!("SELECT id, name, score FROM {table} WHERE id = $1");
    let t0 = Instant::now();
    let workers: Vec<_> = (0..tasks)
        .map(|t| tokio::spawn(pool_worker(pool.clone(), sql.clone(), t, tasks, n, rows)))
        .collect();
    let mut sum = 0;
    for w in workers {
        sum += w.await??;
    }
    report(&format!("postgres.pool_select_{tasks}"), n, sum, t0);
    Ok(())
}

async fn run(c: &mut Client, url: &str, table: &str) -> BenchResult<()> {
    let scale = scale();
    let rows = 20000 / scale;
    insert_tx(c, table, rows).await?;
    point_select(c, table, 20000 / scale, rows).await?;
    range_select(c, table, 5000 / scale, rows).await?;
    let manager = Manager::new(url.parse()?, NoTls);
    let pool = Pool::builder(manager).max_size(POOL_SIZE).build()?;
    // Open every pooled connection before timing (the Node pool is warmed the same way).
    let mut warm = Vec::new();
    for _ in 0..POOL_SIZE {
        warm.push(pool.get().await?);
    }
    drop(warm);
    pool_select(&pool, table, 8, 100000 / scale, rows).await?;
    pool_select(&pool, table, 64, 100000 / scale, rows).await
}

fn main() -> BenchResult<()> {
    db_bench::block_on(bench())
}

async fn bench() -> BenchResult<()> {
    let url = std::env::var("BENCH_PG_URL")?;
    let (mut c, connection) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(connection);
    let table = format!("bench_db_rust_{}", unique_suffix());
    let create = format!(
        "CREATE TABLE {table} (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score DOUBLE PRECISION NOT NULL)"
    );
    c.batch_execute(&create).await?;
    let result = run(&mut c, &url, &table).await;
    c.batch_execute(&format!("DROP TABLE {table}")).await?;
    result
}
