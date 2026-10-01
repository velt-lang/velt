// 10M sequential awaits of an async function that completes immediately.
async fn step(x: i64, i: i64) -> i64 {
    (x * 31 + i) % 1000003
}

fn main() {
    async_bench::run(async {
        let mut acc = 1;
        for i in 0..10_000_000 {
            acc = step(acc, i).await;
        }
        println!("{acc}");
    });
}
