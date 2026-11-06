use ring::agreement::*;
use ring::rand::SystemRandom;
fn main() {
    let n: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    let agree = std::env::args().nth(2).is_some();
    let rng = SystemRandom::new();
    let peer_priv = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
    let peer_pub = peer_priv.compute_public_key().unwrap();
    let peer = UnparsedPublicKey::new(&X25519, peer_pub.as_ref().to_vec());
    let mut acc = 0u8;
    for _ in 0..n {
        let k = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
        if agree {
            acc ^= agree_ephemeral(k, &peer, |s| s[0]).unwrap();
        } else {
            acc ^= k.compute_public_key().unwrap().as_ref()[0];
        }
    }
    println!("{acc}");
}
