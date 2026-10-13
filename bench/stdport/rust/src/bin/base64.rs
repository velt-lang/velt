use base64::{engine::general_purpose::STANDARD, Engine};

fn main() {
    let mut data = vec![0u8; 1048576];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut total: i64 = 0;
    for round in 0..30usize {
        let text = STANDARD.encode(&data);
        let back = STANDARD.decode(&text).unwrap();
        total += (text.len() + back.len()) as i64 + back[round] as i64;
    }
    let mut small = vec![0u8; 32];
    for round in 0..500_000usize {
        small[round % 32] = round as u8;
        let text = STANDARD.encode(&small);
        let back = STANDARD.decode(&text).unwrap();
        total += (text.len() + back.len()) as i64 + back[round % 32] as i64;
    }
    println!("{total}");
}
