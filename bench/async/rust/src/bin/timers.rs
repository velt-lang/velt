// 100k concurrent 1 ms sleeps, joined (not spawned, like the Velt version).
use futures::future::join_all;
use std::time::Duration;

async fn nap(i: i64) -> i64 {
    tokio::time::sleep(Duration::from_millis(1)).await;
    i % 13
}

fn main() {
    async_bench::run(async {
        let rs = join_all((0..100_000).map(nap)).await;
        let total: i64 = rs.into_iter().sum();
        println!("{total}");
    });
}
