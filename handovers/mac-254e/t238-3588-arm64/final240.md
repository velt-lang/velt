#3588 arm64 at 37ce5916b (one-asm reduction) against main 70bb0f818, measured natively on Apple Silicon. Release builds; instructions retired via /usr/bin/time -l; VELT_THREADS=1; 5 runs of each, interleaved, min shown (medians agree within 0.4 points). Main's binaries are the ones from #238, run again now. Every binary prints the same values on both sides.

| Row | main | 37ce5916b | vs main |
|---|---:|---:|---:|
| TLS handshake, verify: false (200) | 566.3M | 560.6M | −1.01% |
| TLS handshake, verified (200) | 906.6M | 874.8M | −3.51% |
| ECDSA P-256 verify (300) | 265.5M | 237.6M | −10.51% |
| ECDH P-256 (300) | 280.1M | 256.3M | −8.46% |
| X25519 (1000) | 382.7M | 383.6M | +0.23% |

**Bar:** every row is within ±3% of main or better. Nothing is over: ECDH went from +3.19% at 2fccd798e to −8.46%, and verify from −1.08% to −10.51%.

**Tests at 37ce5916b, natively (release):**
- `cargo test -p veltc --test p256_masks`: 3 passed;
- `velt_codegen_llvm` field tests: 7 passed, including `the_arm64_choice_is_csels_in_assembly`.

The target and the worktree are deleted.
