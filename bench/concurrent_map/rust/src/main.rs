//! bbtree's concurrent `BBTreeMap` on map.vlt's workloads: the same keys, the same lookups,
//! the same start barrier, so both print the same checksums.
use bbtree::BBTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::*};
use std::time::Instant;

const N: u64 = 10_000_000;
const OPS: u64 = 5_000_000;

fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

struct Barrier {
    ready: AtomicU32,
    go: AtomicBool,
}

impl Barrier {
    fn new() -> Self {
        Barrier { ready: AtomicU32::new(0), go: AtomicBool::new(false) }
    }
    fn arrive(&self) {
        self.ready.fetch_add(1, SeqCst);
        while !self.go.load(Acquire) {
            std::hint::spin_loop();
        }
    }
    fn release(&self, n: u32) -> Instant {
        while self.ready.load(Acquire) < n {
            std::thread::yield_now();
        }
        let start = Instant::now();
        self.go.store(true, Release);
        start
    }
}

fn last(start: Instant, ends: Vec<Instant>) -> f64 {
    ends.into_iter().map(|e| e.duration_since(start).as_secs_f64() * 1e3).fold(0.0, f64::max)
}

fn main() {
    for threads in [1u64, 2, 4, 8] {
        let map: BBTreeMap<u64, u64> = BBTreeMap::new();
        let b = Barrier::new();
        let ms = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (map, b) = (&map, &b);
                    sc.spawn(move || {
                        b.arrive();
                        let mut i = t;
                        while i < N {
                            map.insert(mix(2 * i), i);
                            i += threads;
                        }
                        Instant::now()
                    })
                })
                .collect();
            let start = b.release(threads as u32);
            last(start, hs.into_iter().map(|h| h.join().unwrap()).collect())
        });
        println!("insert {threads} {:.2} 1", N as f64 / ms / 1000.0);

        let b = Barrier::new();
        let (ms, check) = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (map, b) = (&map, &b);
                    sc.spawn(move || {
                        b.arrive();
                        let mut s = mix(t + 12345);
                        let mut check = 0u64;
                        for _ in 0..OPS {
                            s ^= s << 13;
                            s ^= s >> 7;
                            s ^= s << 17;
                            let k = mix(2 * (s % N) + (s >> 63));
                            check = check.wrapping_add(map.get_copied(&k).unwrap_or(1));
                        }
                        (Instant::now(), check)
                    })
                })
                .collect();
            let start = b.release(threads as u32);
            let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
            let check = res.iter().fold(0u64, |a, r| a.wrapping_add(r.1));
            (last(start, res.into_iter().map(|r| r.0).collect()), check)
        });
        println!("get_half {threads} {:.2} {check}", (OPS * threads) as f64 / ms / 1000.0);

        let b = Barrier::new();
        let (ms, check) = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|t| {
                    let (map, b) = (&map, &b);
                    sc.spawn(move || {
                        b.arrive();
                        let mut s = mix(t + 12345);
                        let mut check = 0u64;
                        let mut i = 0;
                        while i < OPS {
                            let keys: [u64; 64] = std::array::from_fn(|_| {
                                s ^= s << 13;
                                s ^= s >> 7;
                                s ^= s << 17;
                                mix(2 * (s % N) + (s >> 63))
                            });
                            let refs: [&u64; 64] = std::array::from_fn(|j| &keys[j]);
                            for v in map.get_many_copied(refs) {
                                check = check.wrapping_add(v.unwrap_or(1));
                            }
                            i += 64;
                        }
                        (Instant::now(), check)
                    })
                })
                .collect();
            let start = b.release(threads as u32);
            let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
            let check = res.iter().fold(0u64, |a, r| a.wrapping_add(r.1));
            (last(start, res.into_iter().map(|r| r.0).collect()), check)
        });
        println!("get_half_batch {threads} {:.2} {check}", (OPS * threads) as f64 / ms / 1000.0);
        for mixed in [false, true] {
            let b = Barrier::new();
            let (ms, check) = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..threads)
                    .map(|t| {
                        let (map, b) = (&map, &b);
                        sc.spawn(move || {
                            b.arrive();
                            let mut s = mix(t + 54321);
                            let mut check = 0u64;
                            for _ in 0..OPS {
                                s ^= s << 13;
                                s ^= s >> 7;
                                s ^= s << 17;
                                if mixed && s % 10 != 0 {
                                    let k = mix(2 * (s % N) + (s >> 63));
                                    check += u64::from(map.get_copied(&k).is_some());
                                } else {
                                    let k = mix(2 * (s % N));
                                    check += u64::from(map.update(&k, |_, v| *v += 1).is_some());
                                }
                            }
                            (Instant::now(), check)
                        })
                    })
                    .collect();
                let start = b.release(threads as u32);
                let res: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
                let check = res.iter().fold(0u64, |a, r| a.wrapping_add(r.1));
                (last(start, res.into_iter().map(|r| r.0).collect()), check)
            });
            let name = if mixed { "mixed" } else { "update" };
            println!("{name} {threads} {:.2} {check}", (OPS * threads) as f64 / ms / 1000.0);
        }
    }
}
