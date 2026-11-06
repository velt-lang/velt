use ring::{agreement, rand, signature};
use std::hint::black_box;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let what = args[1].as_str();
    let n: usize = args[2].parse().unwrap();
    let rng = rand::SystemRandom::new();
    match what {
        // Key generation and public key: one base-point multiplication.
        "p256-keygen" => {
            for _ in 0..n {
                let k = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
                black_box(k.compute_public_key().unwrap());
            }
        }
        // Key generation, public key and agreement: base + variable-base multiplication.
        "p256-ecdh" => {
            let peer = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
            let pp = peer.compute_public_key().unwrap();
            let pp = pp.as_ref().to_vec();
            for _ in 0..n {
                let k = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
                black_box(k.compute_public_key().unwrap());
                let pk = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &pp);
                agreement::agree_ephemeral(k, &pk, |s| { black_box(s[0]); }).unwrap();
            }
        }
        "x25519-ecdh" => {
            let peer = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).unwrap();
            let pp = peer.compute_public_key().unwrap();
            let pp = pp.as_ref().to_vec();
            for _ in 0..n {
                let k = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng).unwrap();
                black_box(k.compute_public_key().unwrap());
                let pk = agreement::UnparsedPublicKey::new(&agreement::X25519, &pp);
                agreement::agree_ephemeral(k, &pk, |s| { black_box(s[0]); }).unwrap();
            }
        }
        "verify" => {
            // args: alg, pubkey hex, msg hex, sig hex
            let alg: &dyn signature::VerificationAlgorithm = match args[3].as_str() {
                "p256" => &signature::ECDSA_P256_SHA256_ASN1,
                "p256-384" => &signature::ECDSA_P256_SHA384_ASN1,
                "p384" => &signature::ECDSA_P384_SHA384_ASN1,
                "p384-256" => &signature::ECDSA_P384_SHA256_ASN1,
                "rsa" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "pss" => &signature::RSA_PSS_2048_8192_SHA256,
                "ed" => &signature::ED25519,
                _ => panic!(),
            };
            let h = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i+2], 16).unwrap()).collect::<Vec<u8>>();
            let pk = h(&args[4]); let m = h(&args[5]); let sg = h(&args[6]);
            for _ in 0..n {
                signature::UnparsedPublicKey::new(alg, &pk).verify(black_box(&m), &sg).unwrap();
            }
        }
        "sha512" => {
            let buf = vec![7u8; 65536];
            for _ in 0..n { black_box(ring::digest::digest(&ring::digest::SHA512, black_box(&buf))); }
        }
        "x509" => {
            let h = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i+2], 16).unwrap()).collect::<Vec<u8>>();
            let der = h(&args[3]);
            for _ in 0..n {
                let d = rustls_pki_types::CertificateDer::from(black_box(&der[..]));
                black_box(webpki::EndEntityCert::try_from(&d).unwrap());
            }
        }
        _ => panic!(),
    }
}
