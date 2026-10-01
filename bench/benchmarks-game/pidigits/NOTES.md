# pidigits — port notes

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

## Sources and credits
| file | source | notes |
|---|---|---|
| `../rust/src/bin/pidigits.rs` | [Rust #4](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-rust-4.html) — TeXitoi, Ryohei Machida | unchanged; uses `rug` (GMP), added to `rust/Cargo.toml` (`default-features = false, features = ["integer"]`, builds its own GMP) |
| `../rust/src/bin/pidigits_limbs.rs` | not a Benchmarks Game program | line-by-line Rust transliteration of `main.vlt` (same limb loops, no GMP): isolates Velt codegen from "hand-written limbs vs GMP" |
| `main.go` | [Go #4](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-go-4.html) — Zhao Zhiqiang, Antonio Petri (after pidigits.c by Paolo Bonzini, Sean Bartlett, Michael Mellor) | cgo + GMP; added `#cgo darwin CFLAGS/LDFLAGS` for Homebrew's `/opt/homebrew` (`brew install gmp`); gofmt |
| `main.js` | [Node #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/pidigits-node-2.html) — Isaac Gouy after Alexander Fyodorov | unchanged; JS `BigInt` (the faster Node #4 needs the native `mpzjs` addon, i.e. GMP again) |
| `main.vlt` | port of Node #2 | own `BigInt` class, see below |

`expected-quick.txt` is the official `pidigits-output.txt` (N = 30, so `QUICK_N=30`). Checked
byte-for-byte: every implementation at N = 30; Velt (both backends) and `pidigits_limbs` against
Rust at N = 10000; Velt LLVM against Node at N = 1, 5, 9, 11, 27, 100, 1234, 3001 (partial last
lines are padded with spaces like Node/Rust; Go #4 prints only full lines, which is fine for the
official N).

## Velt port
Velt has no `BigInt`, so `main.vlt` carries a ~190-line `class BigInt` (sign + little-endian base
2^32 magnitude in a `u32[]`, u64 intermediates) with only the in-place operations the spigot
needs: `mulSmall`, `addMul`/`subMul` by a small factor (sign-crossing handled by two's complement of
the limbs), `compare`, `assign`, and `divRemSmallQuotient` (quotient estimated from the top three
limbs in f64, then corrected by at most a subtraction or two). Temporaries are reused, so the loop
allocates nothing after warm-up. Feasible in about an hour; the pitfall was that the spigot's
accumulator goes negative (a pure natural-number class hangs), which JS/GMP hide.

## Timings (Apple M4, N = 10000, best of 5)
Measured while other agents were benchmarking and building on the same machine (load average
25–32 on 10 cores), so walls are inflated and noisy; CPU seconds (user + sys) are the more reliable
column for these single-threaded programs.

| implementation | wall s | CPU s | peak RSS MB | CPU × Rust |
|---|---:|---:|---:|---:|
| Rust #4 (rug / GMP) | 1.46 | 0.77 | 3.1 | 1.00 |
| Rust `pidigits_limbs` (Velt algorithm, u32 limbs) | 2.58 | 2.39 | 2.0 | 3.10 |
| **Velt LLVM** `main` | 3.64 | 2.86 | 2.6 | 3.71 |
| Velt Cranelift `main` | 7.20 | 6.52 | 2.6 | 8.47 |
| Go #4 (cgo GMP) | 0.82 | 0.70 | 5.9 | 0.91 |
| Go #6 (`math/big`; reference, not in the tree) | 1.49 | 1.42 | 10.3 | 1.84 |
| Node #2 (BigInt) | 11.62 | 10.79 | 179.6 | 14.0 |
| Bun #2 (BigInt) | 2.04 | 2.01 | 101.0 | 2.61 |

An earlier, quieter single run: Rust 0.77 s CPU, `pidigits_limbs` 2.58, Velt LLVM 2.73 — i.e.
Velt vs the identical Rust code is 1.06–1.2× depending on the noise.

## Gap vs Rust (3.7×): root cause and proposed fix
Almost all of it is the bignum library, not the compiler:
- **Velt vs identical Rust code (`pidigits_limbs`): ~1.1–1.2×.** The LLVM code of the limb loops is
  good (bounds checks eliminated, `madd` + carry). The one visible difference: in
  `subMagnitude`/`addMagnitude` Velt reloads `other.limbs.ptr` and `other.limbs.length` on every
  iteration because the store to `this.limbs[i]` may alias `other`'s array header:
  ```
  ldr x12, [x21, #0x10]    ; other.limbs.length, every iteration
  ldr x12, [x21, #0x8]     ; other.limbs.ptr, every iteration
  ldr w12, [x12, x9, lsl #2]
  madd x11, x12, x20, x11
  ```
  Rust hoists both: `&BigInt` means "not modified during the call", so the loads are loop
  invariant. In the Velt LLVM IR the receiver is `ptr noalias nonnull dereferenceable(32) %p0`
  but the read-only `other` param is a bare `ptr %p1` (no `readonly`/`noalias`), and `noalias` on
  `this` doesn't cover the limb buffer, which is reached through a pointer loaded from `this`.
  Fix (compiler): the exclusive-access rule already guarantees a borrowed param isn't modified
  through any other argument, so emit `noalias readonly` on read-only object params of methods
  too (docs/internals/contracts/README.md promises `readonly` for read-only params; it is missing on this method
  param), or TBAA so an `i32` store into an array buffer can't clobber an array header.
- **u32 limbs vs GMP (~3×).** GMP uses 64-bit limbs with hand-written `mpn_addmul_1`/`mpn_submul_1`
  assembly: half the limb operations, each a `mul`+`umulh` pair. Pure Velt can't do 64-bit limbs:
  there is no `u128` and no high-multiply intrinsic.

Proposed fixes, in order of value:
1. **std `BigInt`** (TS has `bigint`; a TS developer reaches for `123n` / `BigInt(x)` first — the
   port needed ~190 lines of limb arithmetic instead). Back it by the Rust runtime (e.g. `num-bigint`
   or `malachite`, both pure Rust and permissively licensed; GMP is LGPL) with in-place operators
   (`a *= k`, `a += b * k`) so loops like this one don't allocate. That makes this program ~1× Go
   `math/big` without any codegen work.
2. **`Math.umulh(a: u64, b: u64): u64`** (or `u128`) so libraries written in Velt can use 64-bit
   limbs (≈2× on this kernel).
3. The `noalias`/TBAA fix above (≈10% here; benefits every "modify one object while reading
   another" loop).

Cranelift is 2.3× slower than LLVM here (not investigated; LLVM is the `--release` default).

## Friction a TypeScript developer hits
- **No `BigInt`** (see above): the single biggest porting cost.
- **`array.length` is `usize`, loop counters are `i64`**: `i < xs.length` is a type error; the
  natural fix `i < xs.length as i64` parses as `(i < xs.length) as i64` (TS precedence: `as` binds
  like `<`) and yields a confusing pair of "mismatched types" errors (`expected bool, found i64`).
  The port adds a `get size(): i64` getter. Suggestion: make `length` an `i64` (JS numbers are
  signed anyway), or give the diagnostic a hint to parenthesize / use a getter.
- `parseInt` returns `f64` (JS-exact), so reading N is `parseInt(argv[0]) as i64`.
- No `new Array(n).fill(0)` / `length = n`: growing and truncating a limb array is `push`/`pop` in
  loops.

## Std and runtime round
`main.vlt` is now the Node program on `std/bigint` (runtime-backed, dashu-int), using the in-place
methods where the JS assigns: 2.76× Rust+GMP. The round-5 hand-written limb class moved to
`main_limbs.vlt`, which compares with the Rust `pidigits_limbs` baseline (same limb code).
