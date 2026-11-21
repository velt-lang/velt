// `Map.getUtf8`: the same program as bench/stdport/map_get_utf8.velt, with the runtime's hash
// (velt_rt `hash.rs`) over the key's bytes and a byte comparison, in a table of the same keys.
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::hint::black_box;

const SEED: u64 = 0x243f_6a88_85a3_08d3;
const K: u64 = 0x9e37_79b9_7f4a_7c15;

fn fmix64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^ (h >> 33)
}

fn mix(h: u64, word: u64) -> u64 {
    (h ^ word).wrapping_mul(K).rotate_left(29)
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = SEED ^ (bytes.len() as u64).wrapping_mul(K);
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        h = mix(h, u64::from_le_bytes(c.try_into().unwrap()));
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut tail = [0u8; 8];
        tail[..rest.len()].copy_from_slice(rest);
        h = mix(h, u64::from_le_bytes(tail));
    }
    fmix64(h)
}

#[derive(Default)]
struct RtHasher(u64);

impl Hasher for RtHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0 = hash(bytes);
    }
}

#[derive(PartialEq, Eq)]
struct Key(Vec<u8>);

impl std::hash::Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&self.0);
    }
}

impl std::borrow::Borrow<[u8]> for Key {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

fn main() {
    let mut m: HashMap<Key, i64, BuildHasherDefault<RtHasher>> = HashMap::default();
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for i in 0..1024i64 {
        let k = match i % 4 {
            0 => format!("x-header-name-{i}-with-a-longer-tail"),
            1 => format!("k{i}"),
            _ => format!("päron/{i}"),
        };
        if i % 4 != 3 {
            m.insert(Key(k.clone().into_bytes()), i);
        }
        keys.push(k.into_bytes());
    }
    let mut total: i64 = 0;
    for i in 0..4_000_000 {
        let key: &[u8] = black_box(&keys[i % 1024]);
        total += m.get(key).copied().unwrap_or(1);
    }
    println!("{total}");
}
