//! Redis workloads with the `redis` crate (bench/db/README.md) against $BENCH_REDIS_URL, on one
//! `MultiplexedConnection` (shared by the concurrent tasks, like std/redis and ioredis). Same
//! commands and sizes as bench/db/velt/redis.vlt.

use db_bench::{report, scale, unique_suffix};
use redis::aio::MultiplexedConnection;
use redis::{RedisResult, Value};
use std::time::Instant;

const BATCH: i64 = 100;
const TASKS: i64 = 50;

fn value_length(v: Option<String>) -> i64 {
    v.map_or(0, |s| s.len() as i64)
}

async fn set_seq(c: &mut MultiplexedConnection, p: &str, n: i64, keys: i64) -> RedisResult<()> {
    let t0 = Instant::now();
    let mut ok = 0;
    for i in 0..n {
        let j = i % keys;
        let reply: Option<String> = redis::cmd("SET")
            .arg(format!("{p}{j}"))
            .arg(format!("value-{j}"))
            .query_async(c)
            .await?;
        if reply.is_some() {
            ok += 1;
        }
    }
    report("redis.set_seq", n, ok, t0);
    Ok(())
}

async fn get_seq(c: &mut MultiplexedConnection, p: &str, n: i64, keys: i64) -> RedisResult<()> {
    let t0 = Instant::now();
    let mut sum = 0;
    for i in 0..n {
        let key = format!("{p}{}", (i * 7919) % keys);
        let v: Option<String> = redis::cmd("GET").arg(key).query_async(c).await?;
        sum += value_length(v);
    }
    report("redis.get_seq", n, sum, t0);
    Ok(())
}

async fn pipeline_set(
    c: &mut MultiplexedConnection,
    p: &str,
    n: i64,
    keys: i64,
) -> RedisResult<()> {
    let t0 = Instant::now();
    let mut ok = 0;
    for b in (0..n).step_by(BATCH as usize) {
        let mut pl = redis::pipe();
        for i in b..b + BATCH {
            let j = i % keys;
            pl.cmd("SET")
                .arg(format!("{p}{j}"))
                .arg(format!("value-{j}"));
        }
        let replies: Vec<Value> = pl.query_async(c).await?;
        ok += replies.iter().filter(|r| matches!(r, Value::Okay)).count() as i64;
    }
    report("redis.pipeline_set", n, ok, t0);
    Ok(())
}

async fn pipeline_get(
    c: &mut MultiplexedConnection,
    p: &str,
    n: i64,
    keys: i64,
) -> RedisResult<()> {
    let t0 = Instant::now();
    let mut sum = 0;
    for b in (0..n).step_by(BATCH as usize) {
        let mut pl = redis::pipe();
        for i in b..b + BATCH {
            pl.cmd("GET").arg(format!("{p}{}", (i * 7919) % keys));
        }
        let replies: Vec<Option<String>> = pl.query_async(c).await?;
        sum += replies.into_iter().map(value_length).sum::<i64>();
    }
    report("redis.pipeline_get", n, sum, t0);
    Ok(())
}

async fn get_worker(
    mut c: MultiplexedConnection,
    p: String,
    t: i64,
    n: i64,
    keys: i64,
) -> RedisResult<i64> {
    let mut sum = 0;
    for i in (t..n).step_by(TASKS as usize) {
        let key = format!("{p}{}", (i * 7919) % keys);
        let v: Option<String> = redis::cmd("GET").arg(key).query_async(&mut c).await?;
        sum += value_length(v);
    }
    Ok(sum)
}

async fn concurrent_get(c: &MultiplexedConnection, p: &str, n: i64, keys: i64) -> RedisResult<()> {
    let t0 = Instant::now();
    let tasks: Vec<_> = (0..TASKS)
        .map(|t| tokio::spawn(get_worker(c.clone(), p.to_string(), t, n, keys)))
        .collect();
    let mut sum = 0;
    for task in tasks {
        sum += task.await.expect("task panicked")?;
    }
    report("redis.concurrent_get", n, sum, t0);
    Ok(())
}

async fn cleanup(c: &mut MultiplexedConnection, p: &str, keys: i64) -> RedisResult<()> {
    for b in (0..keys).step_by(1000) {
        let batch: Vec<String> = (b..(b + 1000).min(keys))
            .map(|j| format!("{p}{j}"))
            .collect();
        let _: i64 = redis::cmd("DEL").arg(batch).query_async(c).await?;
    }
    Ok(())
}

fn main() -> RedisResult<()> {
    db_bench::block_on(run())
}

async fn run() -> RedisResult<()> {
    let scale = scale();
    let url = std::env::var("BENCH_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let client = redis::Client::open(url)?;
    let mut c = client.get_multiplexed_async_connection().await?;
    let p = format!("rust-bench-db:{}:", unique_suffix());
    let keys = 20000 / scale;
    set_seq(&mut c, &p, 40000 / scale, keys).await?;
    get_seq(&mut c, &p, 40000 / scale, keys).await?;
    pipeline_set(&mut c, &p, 1000000 / scale, keys).await?;
    pipeline_get(&mut c, &p, 1000000 / scale, keys).await?;
    concurrent_get(&c, &p, 400000 / scale, keys).await?;
    cleanup(&mut c, &p, keys).await
}
