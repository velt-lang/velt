// 1M rounds of joining two small async functions that return `Result` (they never fail): the
// Rust shape of `await Promise.all([a(), b()])` over promises that can reject.
use futures::future::try_join;

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
            let (a, b) = try_join(task(round, 1), task(round, 2)).await.expect("no error");
            total += a + b;
        }
        println!("{total}");
    });
}
