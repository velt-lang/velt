// 4 producer tasks send 250k numbers each through one bounded tokio mpsc channel (capacity
// 1024) to a consumer that sums them.
use tokio::sync::mpsc;

fn main() {
    async_bench::run(async {
        let (tx, mut rx) = mpsc::channel::<i64>(1024);
        for id in 0..4i64 {
            let tx = tx.clone();
            tokio::spawn(async move {
                for i in 0..250_000 {
                    tx.send(id * 250_000 + i).await.expect("receiver alive");
                }
            });
        }
        drop(tx);
        let (mut total, mut count) = (0i64, 0i64);
        while let Some(v) = rx.recv().await {
            total += v;
            count += 1;
        }
        println!("{count} {total}");
    });
}
