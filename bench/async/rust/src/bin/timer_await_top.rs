// timer_await.rs (Rust has no top level: the same loop).
use std::time::Duration;

fn main() {
    async_bench::run(async {
        let mut s: i64 = 0;
        for i in 0..20_000i64 {
            tokio::time::sleep(Duration::from_millis(1)).await;
            s += i % 7;
        }
        println!("{s}");
    });
}
