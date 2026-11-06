# TLS and AES handover files (#2392)

The patches and benchmark tools from main-95's TLS and AES work, put here when the coordinator took the work over on 2026-11-06. This branch has no pull request and isn't meant to merge. The work-in-progress code is on its own branches: `tls-p256-basetable`, `tls-p384` and `inline-budget` (see the comment on #2392).

## patches/
- `aes2417-commits/0001-0003`: #2417's two commits rebased onto main 8ada30432 (the AES asserts moved to `velt_vir/src/simd/tests.rs`), plus `of_symbol` without allocating (review item 2). Superseded by what was pushed to #2417 (cd9ef32d6 and later).
- `aes2417-uncommitted.patch`, on top of those three:
  - a table-free u8x16 AES emulation: the Boyar-Peralta S-box on eight bit planes, InvSubBytes as L(S(L(x) ^ 05)) ^ 05, xtime by mask;
  - the veltc test `the_aes_checks_follow_the_cpu_and_velt_simd_disable`;
  - the test `the_aes_targets_cover_the_carry_less_ones_and_not_the_reverse`;
  - a golden change and the doc rewording.

  Flat by cachegrind on arm64 for 2,000 rounds, inputs 00 and ff: `aesenc` 758,071 in LLVM release and 1,361,837 in Cranelift; `aesdec` 828,072 and 1,827,837; `aesimc` 492,072 and 873,837. The old table emulation gave `aesimc` 3,062,460 against 14,460,460.
- `tls2413-record-layer.patch`: an independent fix of #2413's ten review items, on c732cf9c8. Superseded by 12d87569e and b28286926. Its expected values may still be useful: master secret `18df0684…`, client Finished key `b80ad010…`, verify_data `a8ec436d…`, a nonce at sequence 0x0102030405060708, and ChaCha20-Poly1305 vectors.

## tools/
- `instr.sh <cmd…>`: best of three "instructions retired" (macOS `/usr/bin/time -l`).
- `mutate.py <test> <mutations>`: applies each `path@@@old@@@new` mutation alone, runs the test, prints KILLED or SURVIVED, and restores the file.
- `gen_ecdh.py`, `gen_ecdsa.py`: test vectors for P-256 ECDH and ECDSA. `rfc8448.py`: the RFC 8448 trace's values.
- `ringbench-tls/`: ring 0.17.14 / webpki benches for X.509 parsing, P-256 ECDH, key generation and ECDSA verify. `ringbench-x25519/`: ring's X25519 `agree_ephemeral`. Copy each out of the repository before `cargo run --release`.
- `inline/`: the #2011 measurements.
  - `count.sh` and `perop.sh` give X25519 instructions per operation.
  - `buildcost.sh` gives a release build's budget, IR size and compiler cost; set `VELT_INLINE_BUDGET` with the `inline-budget` branch.
  - `findings.md` holds the results posted on #2011.
- `aes-flatness/`: `flat.vlt` and the VM scripts that measured the emulation's flatness under cachegrind (OrbStack VM). Set `WT` to a velt worktree.
