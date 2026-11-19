// 1000 rounds of settling 1000 small async tasks, every 7th of which fails: `join_all` over
// futures returning `Result`, each outcome read (allSettled's job). Like Velt's promises, the
// futures start when `join_all` polls them (no spawn).
use futures::future::join_all;

#[derive(Debug)]
struct Failed;

async fn task(round: i64, i: i64) -> Result<i64, Failed> {
    if i % 7 == 3 {
        return Err(Failed);
    }
    Ok((round * 1000 + i) % 7919)
}

fn main() {
    async_bench::run(async {
        let mut total = 0;
        let mut failed = 0;
        for round in 0..1000 {
            let ps: Vec<_> = (0..1000).map(|i| task(round, i)).collect();
            for r in join_all(ps).await {
                match r {
                    Ok(v) => total += v,
                    Err(Failed) => failed += 1,
                }
            }
        }
        println!("{total} {failed}");
    });
}
