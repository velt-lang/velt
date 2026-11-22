//! `mutex_with.velt` on `std::sync::Mutex`: 20M uncontended locks, each adding one.
use std::hint::black_box;
use std::sync::Mutex;

fn main() {
    let m = black_box(Mutex::new(0i64));
    for _ in 0..20_000_000 {
        *m.lock().unwrap() += 1;
    }
    println!("{}", *m.lock().unwrap());
}
