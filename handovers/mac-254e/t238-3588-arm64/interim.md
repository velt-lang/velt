#3588 arm64 (Apple Silicon), 4959a961a -> 626d56c13, interim. Release builds, one target; instructions retired via /usr/bin/time -l; VELT_THREADS=1; 5 runs each, interleaved; the Mac was loaded (counts are instructions, so load matters little).

| Row | base (min) | head (min) | change (min) | change (median) |
|---|---:|---:|---:|---:|
| tls_handshake (bench/crypto, verify: false; 200 handshakes) | 565.9M | 568.5M | +0.45% | +0.47% |
| tls_handshake verified (same, with ca: TEST_CA) | 904.7M | 936.5M | **+3.52%** | **+3.43%** |
| ECDH P-256 (300 agreements) | 264.5M | 291.0M | **+10.05%** | **+9.87%** |
| X25519 (1000 agreements) | 383.3M | 383.5M | +0.03% | +0.01% |
| ECDSA P-256 verify | pending (my first program used an internal import and didn't build; rerunning) | | | |

**Over the ±3% bar:** two rows, ECDH P-256 at +10% and the verified handshake at +3.5%. The handshake's extra cost is about what its P-256 work (the certificate's ECDSA verify) would add at a 10% higher field cost. X25519 is flat, so the cost is in the P-256 portable helpers' `choose`: the mask, the empty-asm barrier, and the xor/and/xor per i128 limb, instead of a `select` (probably a `csel` on arm64).

Both binaries print the same values (ECDH 39505, X25519 113709, 400 per handshake bench).

Still running: p256_masks and the codegen field tests at the new head, then the verify row. The rest comes in the final report.
