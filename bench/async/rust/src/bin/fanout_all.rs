// 1000 rounds of joining 1000 small async tasks. Like Velt' lazy promises, the futures start when
// `join_all` polls them (no spawn).
use futures::future::join_all;

async fn task(round: i64, i: i64) -> i64 {
    (round * 1000 + i) % 7919
}

fn main() {
    async_bench::run(async {
        let mut total = 0;
        for round in 0..1000 {
            let ps: Vec<_> = (0..1000).map(|i| task(round, i)).collect();
            let rs = join_all(ps).await;
            for r in rs {
                total += r;
            }
        }
        println!("{total}");
    });
}
