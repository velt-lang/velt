// 1M sequential awaits of a value that is ready at once.
fn main() {
    async_bench::run(async {
        let mut s: i64 = 0;
        for i in 0..1_000_000i64 {
            s += std::future::ready(i % 7).await;
        }
        println!("{s}");
    });
}
