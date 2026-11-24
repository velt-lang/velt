#3588 on arm64 (Apple Silicon, native): four compilers, five rows. One target; release builds; instructions retired via /usr/bin/time -l; VELT_THREADS=1; 5 runs of each binary, interleaved, min shown (medians agree within 0.2%). Every binary prints the same values.

| Row | main 70bb0f818 | prev head 4959a961a | 626d56c13 | **2fccd798e** | 2fccd798e vs main | 2fccd798e vs prev head |
|---|---:|---:|---:|---:|---:|---:|
| TLS handshake, verify: false (bench/crypto, 200) | 569.7M | 565.6M | 568.1M | 566.6M | −0.55% | +0.18% |
| TLS handshake, verified (ca: TEST_CA, 200) | 910.1M | 903.4M | 935.7M | 910.3M | +0.02% | +0.77% |
| ECDSA P-256 verify (300, RFC 6979 sig) | 265.3M | 257.2M | 280.1M | 262.5M | −1.08% | +2.06% |
| **ECDH P-256 (300)** | 280.1M | 264.7M | 290.9M | **289.0M** | **+3.19%** | **+9.19%** |
| X25519 (1000) | 382.6M | 383.4M | 383.5M | 383.3M | +0.18% | −0.02% |

**Over the ±3% bar at 2fccd798e:** ECDH P-256 only. It is +3.19% against main and +9.19% against the previous head.
- 2fccd798e won back almost all of 626d56c13's cost on verify (from +8.9% to +2.1%) and on the verified handshake (from +3.6% to +0.8%).
- On ECDH it won back only 0.7 points: it reads 289.0M against 290.9M at 626d56c13.
- So ECDH's extra cost is on a path the csel change doesn't cover. ECDH is variable-base, and on arm64 the fused doubling returns false and std's own formulas run. My guess, not traced, is a `choose` or a portable form that the variable-base path uses and verify uses less (doubling, or the table select).
- Everything else matches or beats main.

**Tests at 2fccd798e, natively on the Mac (release):**
- `cargo test -p veltc --test p256_masks`: 3 passed;
- `velt_codegen_llvm` field tests: 7 passed, plus `the_arm64_choice_is_csels_in_assembly`: 1 passed.

Programs: bench/crypto/tls_handshake.velt as is, plus four of mine (a copy of it with `ca: TEST_CA`, p256_verify, p256_ecdh, x25519). I can paste them if useful. Target deleted.
