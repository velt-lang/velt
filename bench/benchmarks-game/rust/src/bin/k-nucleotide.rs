// The Computer Language Benchmarks Game
// https://salsa.debian.org/benchmarksgame-team/benchmarksgame/
//
// contributed by Alexander
//
// Source: https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/knucleotide-rust-7.html
// Velt port deviations (the bench project only depends on rayon and regex):
// - `hashbrown::HashMap` (default hasher) -> `std::collections::HashMap` with an inline
//   FxHash-style hasher (hashbrown's aHash/foldhash are likewise fast non-cryptographic hashes);
// - `tokio_threadpool` futures -> `std::thread::scope` (one thread per frame length, as before);
// - `itertools::sorted_by` -> `Vec::sort_by`; `num::FromPrimitive` -> `From<u8>` + a shift.
// k-nucleotide_st.rs runs the same code with the frame lengths counted one after another.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

/// FxHash (rustc's hasher): one multiply per integer key.
#[derive(Default)]
pub struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(i as u64)
    }
    fn write_u16(&mut self, i: u16) {
        self.write_u64(i as u64)
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64)
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

type Map<T> = HashMap<T, u32, BuildHasherDefault<FxHasher>>;

trait ShlXorMsk<T> {
    fn sh(a: T, x: u8, m: T) -> T;
    fn mask(len: usize) -> T;
}

impl ShlXorMsk<u8> for u8 {
    fn sh(a: u8, x: u8, m: u8) -> u8 {
        m & (a << 2) | x
    }
    fn mask(len: usize) -> u8 {
        ((1u16 << 2 * len) - 1) as u8
    }
}

impl ShlXorMsk<u16> for u16 {
    fn sh(a: u16, x: u8, m: u16) -> u16 {
        m & (a << 2) | (x as u16)
    }
    fn mask(len: usize) -> u16 {
        ((1u32 << 2 * len) - 1) as u16
    }
}

impl ShlXorMsk<u32> for u32 {
    fn sh(a: u32, x: u8, m: u32) -> u32 {
        m & (a << 2) | (x as u32)
    }
    fn mask(len: usize) -> u32 {
        ((1u64 << 2 * len) - 1) as u32
    }
}

impl ShlXorMsk<u64> for u64 {
    fn sh(a: u64, x: u8, m: u64) -> u64 {
        m & (a << 2) | (x as u64)
    }
    fn mask(len: usize) -> u64 {
        (1u64 << 2 * len) - 1
    }
}

fn match_key(k: u8) -> char {
    match k {
        0b00 => 'A',
        0b01 => 'C',
        0b10 => 'T',
        0b11 => 'G',
        _ => '_',
    }
}

fn print_stat(h: Map<u8>, seq_len: usize) {
    let total = h.values().sum::<u32>();

    let mut sorted: Vec<(u8, u32)> = h.into_iter().collect();
    sorted.sort_by(|&(ref a, x), &(ref b, y)| {
        let ord1 = Ord::cmp(&y, &x);
        if ord1 == Ordering::Equal {
            Ord::cmp(&b, &a)
        } else {
            ord1
        }
    });
    sorted.into_iter().for_each(|(k, v)| {
        if seq_len == 1 {
            println!("{} {:.3}", match_key(k), (100 * v) as f32 / total as f32);
        } else {
            println!(
                "{}{} {:.3}",
                match_key(k >> 2),
                match_key(0b11 & k),
                (100 * v) as f32 / total as f32
            );
        };
    });
    println!();
}

fn print<T: From<u8> + Default + Hash + Eq + ShlXorMsk<T> + Copy>(h: Map<T>, seq: &str) {
    let mask = T::mask(seq.len());
    let k = seq
        .to_ascii_lowercase()
        .as_bytes()
        .iter()
        .map(|x| 0b11u8 & x >> 1)
        .fold(T::default(), |acc, x| T::sh(acc, x, mask));
    println!("{}\t{}", h.get(&k).unwrap_or(&0), seq);
}

fn freq<T: Default + Hash + Eq + ShlXorMsk<T> + Copy>(s_vec: &[u8], len: usize) -> Map<T> {
    let mut h = Map::default();
    let mask = T::mask(len);
    let mut it = s_vec.iter();
    let mut a = it
        .by_ref()
        .take(len - 1)
        .fold(T::default(), |acc, &x| T::sh(acc, x, mask));
    for &x in it {
        a = T::sh(a, x, mask);
        *h.entry(a).or_insert(0) += 1;
    }
    h
}

fn get_seq<R: std::io::BufRead>(mut r: R, key: &[u8]) -> Vec<u8> {
    let mut res = Vec::with_capacity(65536);
    let mut line = Vec::with_capacity(64);

    loop {
        match r.read_until(b'\n', &mut line) {
            Ok(b) if b > 0 => {
                if line.starts_with(key) {
                    break;
                }
            }
            _ => break,
        }
        line.clear();
    }

    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(b) if b > 0 => res.extend(line[..line.len() - 1].iter().map(|&x| 0b11 & x >> 1)),
            _ => break,
        }
    }

    res
}

pub fn calc<R: std::io::BufRead>(r: R, parallel: bool) {
    let s = get_seq(r, b">THREE");
    let s = &s[..];

    std::thread::scope(|scope| {
        macro_rules! job {
            ($t:ty, $len:expr) => {{
                if parallel {
                    let h = scope.spawn(move || freq::<$t>(s, $len));
                    Box::new(move || h.join().unwrap()) as Box<dyn FnOnce() -> Map<$t> + '_>
                } else {
                    Box::new(move || freq::<$t>(s, $len)) as Box<dyn FnOnce() -> Map<$t> + '_>
                }
            }};
        }
        let f7 = job!(u64, 18);
        let f6 = job!(u32, 12);
        let f5 = job!(u16, 6);
        let f4 = job!(u8, 4);
        let f3 = job!(u8, 3);
        let f2 = job!(u8, 2);
        let f1 = job!(u8, 1);
        print_stat(f1(), 1);
        print_stat(f2(), 2);
        print::<u8>(f3(), "GGT");
        print::<u8>(f4(), "GGTA");
        print::<u16>(f5(), "GGTATT");
        print::<u32>(f6(), "GGTATTTTAATT");
        print::<u64>(f7(), "GGTATTTTAATTTATAGT");
    });
}

#[allow(dead_code)] // also compiled as a module of k-nucleotide_st.rs
fn main() {
    let stdin = std::io::stdin();
    calc(stdin.lock(), true);
}
