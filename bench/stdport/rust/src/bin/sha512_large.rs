use sha2::{Digest, Sha512};

fn main() {
    let mut data = vec![0u8; 1048576];
    for (i, b) in data.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let mut digest = [0u8; 64];
    for _ in 0..20 {
        digest = Sha512::digest(&data).into();
        data[..64].copy_from_slice(&digest);
    }
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
