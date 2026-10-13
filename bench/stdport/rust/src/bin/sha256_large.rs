use sha2::{Digest, Sha256};

fn main() {
    let mut data = vec![0u8; 1048576];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut digest = [0u8; 32];
    for _ in 0..40 {
        digest = Sha256::digest(&data).into();
        data[..32].copy_from_slice(&digest);
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
