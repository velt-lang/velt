use std::hint::black_box;

fn main() {
    let mut data = vec![0u8; 1048576];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 131 + 7) as u8;
    }
    let mut total: i64 = 0;
    for round in 0..60usize {
        data[round] = round as u8;
        let text = hex::encode(black_box(&data));
        let back = hex::decode(black_box(&text)).unwrap();
        total += (text.len() + back.len()) as i64 + back[round] as i64;
    }
    let mut small = vec![0u8; 32];
    for round in 0..2_000_000usize {
        small[round % 32] = round as u8;
        let text = hex::encode(black_box(&small));
        let back = hex::decode(black_box(&text)).unwrap();
        total += (text.len() + back.len()) as i64 + back[round % 32] as i64;
    }
    println!("{total}");
}
