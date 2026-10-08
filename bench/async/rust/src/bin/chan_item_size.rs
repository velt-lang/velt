// `send` + `try_recv` of 1M items on one task through an unbounded tokio mpsc channel, twice:
// `i64`s, then 96-byte structs.
use tokio::sync::mpsc;

// Only `l` is read back; the rest is the item's size.
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct Big {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
    i: f64,
    j: f64,
    k: f64,
    l: f64,
}

fn small(n: i64) -> i64 {
    let (tx, mut rx) = mpsc::unbounded_channel::<i64>();
    let mut sum = 0;
    for i in 0..n {
        tx.send(i).expect("receiver alive");
        sum += rx.try_recv().unwrap_or(0);
    }
    sum
}

fn big(n: i64) -> f64 {
    let (tx, mut rx) = mpsc::unbounded_channel::<Big>();
    let mut sum = 0.0;
    for i in 0..n {
        let x = i as f64;
        let v = Big {
            a: x,
            b: x,
            c: x,
            d: x,
            e: x,
            f: x,
            g: x,
            h: x,
            i: x,
            j: x,
            k: x,
            l: x,
        };
        tx.send(v).expect("receiver alive");
        sum += rx.try_recv().map_or(0.0, |b| b.l);
    }
    sum
}

fn main() {
    async_bench::run(async {
        println!("{} {}", small(1_000_000), big(1_000_000));
    });
}
