use sha2::{Digest, Sha256};

fn main() {
    let mut message = vec![0u8; 48];
    for (i, b) in message.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut digest = [0u8; 32];
    for _ in 0..500_000 {
        digest = Sha256::digest(&message).into();
        message[..32].copy_from_slice(&digest);
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
