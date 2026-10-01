// A recursive async function (depth 20) awaited 500k times. A recursive async fn needs its
// recursive call boxed (its future would otherwise contain itself): one allocation per level.
async fn deep(n: i64, x: i64) -> i64 {
    if n == 0 {
        return x;
    }
    let r = Box::pin(deep(n - 1, x + n)).await;
    (r * 7 + 1) % 1000003
}

fn main() {
    async_bench::run(async {
        let mut acc = 0;
        for i in 0..500_000 {
            acc = (acc + deep(20, i).await) % 1000003;
        }
        println!("{acc}");
    });
}
