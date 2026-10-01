// An await-free hot loop inside an async function (see hot_loop.vlt).
async fn seed(round: i64) -> i64 {
    (round * 7919 + 17) % 65537
}

fn main() {
    async_bench::run(async {
        let mut total: i64 = 0;
        let mut kept: usize = 0;
        let mut bytes: Vec<u8> = Vec::new();
        for round in 0..40 {
            let mut x = seed(round).await;
            for i in 0..5_000_000 {
                x = (x + i * 7) & 65535;
                let b = x & 255;
                if b != 10 {
                    bytes.push(b as u8);
                }
                total += b;
            }
            kept += bytes.len();
            bytes = Vec::new();
        }
        println!("{total} {kept}");
    });
}
