use sha2::{Digest, Sha384};

fn main() {
    let mut message = vec![0u8; 48];
    for (i, b) in message.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut digest = [0u8; 48];
    for _ in 0..500_000 {
        digest = Sha384::digest(&message).into();
        message[..48].copy_from_slice(&digest);
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
