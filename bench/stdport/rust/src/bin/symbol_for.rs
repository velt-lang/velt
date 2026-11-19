use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Mutex;

/// A record: its key (the description) and that it is registered.
struct Rec {
    key: String,
}

static REGISTRY: Mutex<Option<HashMap<Vec<u8>, usize>>> = Mutex::new(None);

fn symbol_for(key: &str) -> usize {
    let mut reg = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    let map = reg.get_or_insert_with(HashMap::new);
    if let Some(&rec) = map.get(key.as_bytes()) {
        return rec;
    }
    let rec = Box::into_raw(Box::new(Rec {
        key: key.to_string(),
    })) as usize;
    map.insert(key.as_bytes().to_vec(), rec);
    rec
}

fn key_for(s: usize) -> String {
    // SAFETY: `s` is a record `symbol_for` leaked.
    unsafe { (*(s as *const Rec)).key.clone() }
}

fn main() {
    let keys: Vec<String> = (0..64).map(|i| format!("app.key.{i}")).collect();
    let mut total: i64 = 0;
    let first = symbol_for(&keys[0]);
    for round in 0..2_000_000usize {
        if symbol_for(black_box(&keys[round % 64])) == first {
            total += 1;
        }
    }
    for i in 0..20_000 {
        let s = symbol_for(&format!("fresh.{i}"));
        total += key_for(s).len() as i64;
    }
    for round in 0..1_000_000usize {
        let k = key_for(symbol_for(black_box(&keys[round % 64])));
        total += k.len() as i64;
    }
    println!("{total}");
}
