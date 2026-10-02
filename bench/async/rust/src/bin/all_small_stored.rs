// 1M rounds of joining a vector of three small async calls returning `Result` (they never
// fail): the Rust shape of `Promise.all` over a stored array of three promises.
use futures::future::try_join_all;

async fn task(round: i64, i: i64) -> Result<i64, String> {
    if round < 0 {
        return Err("never".into());
    }
    Ok((round * 1000 + i) % 7919)
}

fn main() {
    async_bench::run(async {
        let mut total = 0;
        for round in 0..1_000_000 {
            let ps = vec![task(round, 1), task(round, 2), task(round, 3)];
            let rs = try_join_all(ps).await.expect("no error");
            total += rs[0] + rs[1] + rs[2];
        }
        println!("{total}");
    });
}
