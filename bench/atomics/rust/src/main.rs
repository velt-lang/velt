//! The workloads of bench/atomics/atomics.vlt on `std::sync::atomic` and `std::thread`, with
//! the same latch word and backoff as velt:sync (bbtree's `NodeLatch`). Each line is
//! `name ms checksum`; a threaded workload's time runs from when every worker is ready to when
//! the last one finishes, as in atomics.vlt.

use std::hint::spin_loop;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::thread;
use std::time::Instant;

const THREADS: usize = 4;
const STRIDE: usize = 16;

const EXCLUSIVE: u64 = 1;
const SHARED_MASK: u64 = ((1 << 20) - 1) << 1;
const VERSION_UNIT: u64 = 1 << 21;
const SPIN_ROUNDS: u32 = 10;

struct Backoff(u32);

impl Backoff {
    fn spin(&mut self) {
        for _ in 0..1u32 << self.0 {
            spin_loop();
        }
        if self.0 < SPIN_ROUNDS {
            self.0 += 1;
        }
    }
    fn snooze(&mut self) {
        if self.0 < SPIN_ROUNDS {
            self.spin();
        } else {
            thread::yield_now();
        }
    }
}

struct Latch(AtomicU64);

impl Latch {
    fn read_version(&self) -> u64 {
        let w = self.0.load(Acquire);
        if w & EXCLUSIVE == 0 {
            return w & !SHARED_MASK;
        }
        let mut b = Backoff(0);
        loop {
            b.spin();
            let w = self.0.load(Acquire);
            if w & EXCLUSIVE == 0 {
                return w & !SHARED_MASK;
            }
        }
    }
    fn is_unchanged(&self, v: u64) -> bool {
        fence(Acquire);
        self.0.load(Relaxed) & !SHARED_MASK == v
    }
    fn try_lock_at(&self, v: u64) -> bool {
        let locked = self.0.compare_exchange(v, v | EXCLUSIVE, Acquire, Relaxed).is_ok();
        fence(Release);
        locked
    }
    fn lock_exclusive(&self) -> u64 {
        let mut b = Backoff(0);
        loop {
            let w = self.0.load(Relaxed);
            if w & (EXCLUSIVE | SHARED_MASK) == 0 && self.try_lock_at(w) {
                return w;
            }
            b.snooze();
        }
    }
    fn unlock_exclusive(&self, v: u64) {
        self.0.store(v + VERSION_UNIT, Release);
    }
}

struct Words {
    w: Vec<AtomicU64>,
    latch: Latch,
    lock: AtomicU32,
    stop: AtomicBool,
    ready: AtomicU32,
    go: AtomicBool,
}

fn words(n: usize) -> Words {
    Words {
        w: (0..n).map(|_| AtomicU64::new(0)).collect(),
        latch: Latch(AtomicU64::new(0)),
        lock: AtomicU32::new(0),
        stop: AtomicBool::new(false),
        ready: AtomicU32::new(0),
        go: AtomicBool::new(false),
    }
}

fn report(name: &str, ms: f64, check: u64) {
    println!("{name} {ms:.1} {check}");
}

fn arrive(s: &Words) {
    s.ready.fetch_add(1, SeqCst);
    while !s.go.load(Acquire) {
        spin_loop();
    }
}

fn release(s: &Words, n: u32) -> Instant {
    while s.ready.load(Acquire) < n {
        thread::yield_now();
    }
    let start = Instant::now();
    s.go.store(true, Release);
    start
}

fn span(start: Instant, ends: impl Iterator<Item = Instant>) -> f64 {
    let last = ends.fold(start, |a, b| a.max(b));
    (last - start).as_secs_f64() * 1000.0
}

fn add_one(n: u64) {
    let s = words(1);
    let start = Instant::now();
    for _ in 0..n {
        s.w[0].fetch_add(1, Relaxed);
    }
    report("add_1t", start.elapsed().as_secs_f64() * 1000.0, s.w[0].load(SeqCst));
}

fn cas_one(n: u64) {
    let s = words(1);
    let start = Instant::now();
    for _ in 0..n {
        let mut v = s.w[0].load(Relaxed);
        while s.w[0]
            .compare_exchange_weak(v, v + 1, AcqRel, Relaxed)
            .is_err()
        {
            v = s.w[0].load(Relaxed);
        }
    }
    report("cas_loop_1t", start.elapsed().as_secs_f64() * 1000.0, s.w[0].load(SeqCst));
}

fn add_many(name: &str, n: u64, shard: bool) {
    let s = words(THREADS * STRIDE);
    let ms = thread::scope(|sc| {
        let hs: Vec<_> = (0..THREADS)
            .map(|t| {
                let s = &s;
                sc.spawn(move || {
                    let at = if shard { t * STRIDE } else { 0 };
                    arrive(s);
                    for _ in 0..n {
                        s.w[at].fetch_add(1, Relaxed);
                    }
                    Instant::now()
                })
            })
            .collect();
        let start = release(&s, THREADS as u32);
        span(start, hs.into_iter().map(|h| h.join().unwrap()))
    });
    let sum = (0..THREADS).map(|t| s.w[t * STRIDE].load(SeqCst)).sum();
    report(name, ms, sum);
}

fn read_loop(s: &Words, n: u64) -> u64 {
    let mut torn = 0u64;
    for _ in 0..n {
        loop {
            let v = s.latch.read_version();
            let a = s.w[0].load(Relaxed);
            let b = s.w[1].load(Relaxed);
            if s.latch.is_unchanged(v) {
                torn += u64::from(a != b);
                break;
            }
        }
    }
    torn
}

fn latch_reads(name: &str, n: u64, writer: bool) {
    let s = words(2);
    thread::scope(|sc| {
        let w = writer.then(|| {
            sc.spawn(|| {
                arrive(&s);
                while !s.stop.load(Relaxed) {
                    let v = s.latch.lock_exclusive();
                    s.w[0].store(s.w[0].load(Relaxed) + 1, Relaxed);
                    s.w[1].store(s.w[1].load(Relaxed) + 1, Relaxed);
                    s.latch.unlock_exclusive(v);
                    for _ in 0..64 {
                        spin_loop();
                    }
                }
            })
        });
        let readers: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    arrive(&s);
                    let torn = read_loop(&s, n);
                    (Instant::now(), torn)
                })
            })
            .collect();
        let start = release(&s, (THREADS + usize::from(writer)) as u32);
        let done: Vec<(Instant, u64)> = readers.into_iter().map(|r| r.join().unwrap()).collect();
        let torn = done.iter().map(|d| d.1).sum();
        report(name, span(start, done.iter().map(|d| d.0)), torn);
        s.stop.store(true, Relaxed);
        if let Some(w) = w {
            w.join().unwrap();
        }
    });
}

fn spin_lock(n: u64) {
    let s = words(1);
    let ms = thread::scope(|sc| {
        let hs: Vec<_> = (0..THREADS)
            .map(|_| {
                sc.spawn(|| {
                    arrive(&s);
                    for _ in 0..n {
                        let mut b = Backoff(0);
                        while s.lock.compare_exchange_weak(0, 1, Acquire, Relaxed).is_err() {
                            b.snooze();
                        }
                        s.w[0].store(s.w[0].load(Relaxed) + 1, Relaxed);
                        s.lock.store(0, Release);
                    }
                    Instant::now()
                })
            })
            .collect();
        let start = release(&s, THREADS as u32);
        span(start, hs.into_iter().map(|h| h.join().unwrap()))
    });
    report("spinlock_4t", ms, s.w[0].load(SeqCst));
}

fn main() {
    add_one(200_000_000);
    cas_one(50_000_000);
    add_many("add_contended_4t", 10_000_000, false);
    add_many("add_sharded_4t", 50_000_000, true);
    latch_reads("latch_reads_4t", 10_000_000, true);
    latch_reads("latch_reads_quiet_4t", 10_000_000, false);
    spin_lock(2_000_000);
}
