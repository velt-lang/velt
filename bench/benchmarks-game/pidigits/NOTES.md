# pidigits: port notes

## Sources and credits
| file | source | notes |
|---|---|---|
| `../rust/src/bin/pidigits.rs` | [Rust #4](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-rust-4.html) — TeXitoi, Ryohei Machida | unchanged; uses `rug` (GMP), added to `rust/Cargo.toml` (`default-features = false, features = ["integer"]`, builds its own GMP) |
| `../rust/src/bin/pidigits_limbs.rs` | not a Benchmarks Game program | line-by-line Rust transliteration of `main_limbs.velt` (same limb loops, no GMP): isolates Velt codegen from "hand-written limbs vs GMP" |
| `main.go` | [Go #4](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-go-4.html) — Zhao Zhiqiang, Antonio Petri (after pidigits.c by Paolo Bonzini, Sean Bartlett, Michael Mellor) | cgo + GMP; added `#cgo darwin CFLAGS/LDFLAGS` for Homebrew's `/opt/homebrew` (`brew install gmp`); gofmt |
| `main.js` | [Node #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-node-2.html) — Isaac Gouy after Alexander Fyodorov | unchanged; JS `BigInt` (the faster Node #4 needs the native `mpzjs` addon, i.e. GMP again) |
| `main.velt` | port of Node #2 | `BigInt` from `velt/math`, in-place methods where the JS assigns |
| `main_limbs.velt` | port of Node #2 | a hand-written limb class (base 2^32 in a `u32[]`); compare with `pidigits_limbs` |

`expected-quick.txt` is the official `pidigits-output.txt` (N = 30, so `QUICK_N=30`). Checked
byte-for-byte: every implementation at N = 30; Velt (both backends) and `pidigits_limbs` against
Rust at N = 10000; Velt LLVM against Node at N = 1, 5, 9, 11, 27, 100, 1234, 3001 (partial last
lines are padded with spaces like Node/Rust; Go #4 prints only full lines, which is fine for the
official N).

## Where the time goes

Timings are in [../RESULTS.md](../RESULTS.md); the `main` row there was measured on an earlier,
runtime-backed `BigInt`. `velt/math`'s `BigInt` is Velt code on 64-bit limbs (`Math.umulh` gives
the high half of a product) and has not been measured in this suite yet.

- **`main_limbs.velt` against the identical Rust code (`pidigits_limbs`)** is about 1.1×. The
  limb loops compile well (bounds checks eliminated, `madd` + carry), and the read-only `other`
  parameter of `addMagnitude` / `subMagnitude` is `readonly`
  (`crates/velt_vir/src/lower/param_attrs.rs`).
- **Limbs against GMP.** GMP uses 64-bit limbs with hand-written `mpn_addmul_1` /
  `mpn_submul_1` assembly; `main_limbs.velt`'s 32-bit limbs need twice the limb operations.
- The spigot's accumulator goes negative, so a limb class for it needs a sign (a natural-number
  class hangs); JS `BigInt` and GMP hide this.
- **Cranelift** is 2–2.5× slower than LLVM on the limb code.
