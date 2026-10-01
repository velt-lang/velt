// Spawn 1M tasks, each doing trivial work plus one yield_now, then join them all in order.
async fn work(i: i64) -> i64 {
    let v = (i * 17 + 3) % 1009;
    tokio::task::yield_now().await;
    v
}

fn main() {
    async_bench::run(async {
        let handles: Vec<_> = (0..1_000_000).map(|i| tokio::spawn(work(i))).collect();
        let mut total = 0;
        for h in handles {
            total += h.await.expect("task");
        }
        println!("{total}");
    });
}
