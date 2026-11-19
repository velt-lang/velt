use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64() * 1000.0;
    let mut last = now();
    let mut backwards = 0;
    let mut sum: i64 = 0;
    for _ in 0..2_000_000 {
        let t = now();
        if t < last {
            backwards += 1;
        }
        last = t;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        sum += ms % 7;
    }
    println!("{} {}", backwards == 0, sum >= 0);
}
