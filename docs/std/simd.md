# SIMD

```ts
import { u8x16, f32x4, mask8x16 } from "velt:simd";
```

The 128-bit vector types ([SIMD vectors](../reference/types.md#simd-vectors)) and their
methods. A module imports the types it names; the methods work on every vector, also one whose
type the module does not name (the mask `eq` returns). `T` is a vector type
(`i8x16 u8x16 i16x8 u16x8 i32x4 u32x4 i64x2 u64x2 f32x4 f64x2`), `L` the type of its lanes, `n`
their number and `M` its mask type (`mask8x16`, `mask16x8`, `mask32x4`, `mask64x2`: lanes as
wide as the vector's). Each method is one instruction, or a few, on x86-64 (SSE2) and arm64
(NEON) and WebAssembly (`simd128`), in debug builds too. WebAssembly SIMD is part of Velt's
WebAssembly baseline: a program that uses vectors needs an engine that has it (every current
browser, Node.js, wasmtime and wasmer do).

| Member | Meaning |
|---|---|
| `T.splat(x)` | every lane `x` |
| `T.fits(xs, at = 0)` | whether the `n` elements of the `L[]` from `at` are inside it: what `load` and `store` check |
| `T.load(xs, at = 0)` | the `n` elements of the `L[]` from `at`; a RangeError panic unless all are inside the array |
| `v.store(xs, at = 0)` | writes the lanes to the `L[]` from `at`; a RangeError panic unless they fit (the array does not grow) |
| `u8x16.loadString(s, at = 0)` | the bytes of string `s` from byte `at`, up to 16, in the low lanes, 0 in the others: read where the string keeps them, without a copy (its UTF-8, a lone surrogate as its three-byte form; velt:encoding `utf8Length` counts them); `at` may be the byte length (all 0), a RangeError panic past it |
| `T.loadOptimistic(words, at = 0)` | integer vectors: the `n` words of an `Atomic<L>[]` from `at`, read while other threads may be storing to them: a lane no thread wrote during the read is the word, a lane a store raced is some value (no undefined behaviour), so trust the lanes only after a `VersionLatch` check (velt:sync); one vector load where reading the words one by one takes `n` atomic loads; a RangeError panic unless all are inside the array |
| `T.loadPartial(xs, at = 0)` | the elements from `at` that exist, up to `n`, in the low lanes, 0 in the others; `at` may be the length (all 0), a RangeError panic past it |
| `v.storePartial(xs, at = 0)` | writes the low lanes to the elements from `at` that exist; `at` may be the length (nothing written), a RangeError panic past it |
| `T.index()` | each lane its own index: 0, 1, 2, … |
| `v.lane(i)`, `v.withLane(i, x)` | lane `i`; the vector with lane `i` set to `x` (a RangeError panic unless `i < n`) |
| `add` `sub` `mul` | lane by lane; integers wrap (`mul`: not 8-bit lanes) |
| `div` `sqrt` | floats |
| `neg` `abs` | signed integers and floats (`abs`: not 64-bit integers) |
| `min` `max` | not 64-bit integers; floats as `Math.min` and `Math.max` (NaN if either is, `-0` below `0`) |
| `addSat` `subSat` | 8- and 16-bit lanes: clamped to the lane type's range instead of wrapping |
| `and` `or` `xor` `not` | bitwise, integers |
| `shl(count)` `shr(count)` | integers, every lane by `count` bits (taken modulo the lane width); `shr` keeps the sign of signed lanes |
| `eq` `ne` `lt` `le` `gt` `ge` | the mask `M` of the lanes where the comparison holds (with a NaN lane only `ne` holds) |
| `T.select(m, a, b)` | lane by lane, `a` where `m` is set, `b` elsewhere |
| `v.sum()` | the sum of the lanes: 32- and 64-bit lanes |
| `v.zipLow(o)` `v.zipHigh(o)` | the lanes of the low (high) halves of `v` and `o` in turn: `v0, o0, v1, o1, …` |
| `v.widenLow()` `v.widenHigh()` | the low (high) half's lanes at twice the width, `u8x16` to `u16x8`: 8-, 16- and 32-bit integers |
| `v.narrow(o)` | the lanes of `v`, then of `o`, at half the width, clamped to its range, two `u16x8` to a `u8x16`: 16- and 32-bit integers |
| `T.shuffle(a, b, i0, …)` | one lane index per lane: lane `k` is lane `ik` of `a`'s lanes followed by `b`'s (`0` to `n - 1` from `a`, `n` to `2n - 1` from `b`), like WebAssembly's `i8x16.shuffle` and LLVM's `shufflevector`. The indices must be integer literals (a compile error otherwise): they are compiled into the instruction. `u32x4.shuffle(v, v, 1, 0, 3, 2)` swaps neighbouring lanes, `u8x16.shuffle(a, b, 8, 9, …, 23)` is the bytes from the middle of `a`, `b` (`palignr`) |
| `v.reverseBytes()` | the bytes of each lane in reverse order, so big-endian words read with `load` become their values: 16-, 32- and 64-bit integers |
| `toF32x4()` | `i32x4`, `u32x4`: each lane as the nearest `f32` |
| `toI32x4()` `toU32x4()` | `f32x4`: each lane truncated toward zero and clamped to the range, NaN as 0 |
| `v.asU8x16()`, … | the same 128 bits as another vector type, nothing converted: to and from `u8x16`, and between the types with as many lanes |

A mask `m`:

| Member | Meaning |
|---|---|
| `M.prefix(count)` | the mask with its first `count` lanes set |
| `m.any()`, `m.all()` | whether some lane, every lane is set |
| `m.first()` | the index of the first set lane, `n` when none is |
| `m.count()` | the number of set lanes |
| `m.bitmask()` | a `u64` whose bit `i` is lane `i` |
| `and` `or` `xor` `not` | lane by lane |

The methods keep the types they declare: a lane of a `u8x16` is a `u8`, `first()` a `usize`.
A position or lane index (`at` of `fits`, `load`, `store`, `loadPartial` and `storePartial`,
`i` of `lane` and `withLane`)
given as a `number` is used as itself when it is a whole number from 0 up; any other number
(negative, fractional, NaN, ±Infinity) is out of range: `fits` gives `false` and the others
panic with their RangeError. A negative literal written there (`v.lane(-1)`) is a compile
error instead, since the parameter is a `usize`. A lane value or shift count given as a `number` is converted as
the standard library converts every `number` it takes as an integer: truncated, clamped to the
type's range, NaN as 0 (`u8x16.splat(300)` is 255 in every lane).

## SHA instructions

`u32x4` has the CPU's SHA-1 and SHA-256 instructions as methods, named after the instruction,
with its operands in the instruction's order and lane 0 the low 32 bits: the ARMv8 ones as in
ARM's ACLE (`vsha256hq_u32(abcd, efgh, wk)` is `abcd.sha256h(efgh, wk)`) and the x86 ones as in
Intel's intrinsics (`_mm_sha256rnds2_epu32(cdgh, abef, wk)` is `cdgh.sha256rnds2(abef, wk)`), so
published code ports line by line.

| Method | Instruction |
|---|---|
| `abcd.sha1c(e, wk)` `sha1p` `sha1m`, `u32x4.sha1h(e)` | ARM SHA1C, SHA1P, SHA1M (four rounds; `e` a `u32`), SHA1H |
| `w0.sha1su0(w1, w2)`, `t.sha1su1(w3)` | ARM SHA1SU0, SHA1SU1 (W0 in lane 0) |
| `abcd.sha256h(efgh, wk)`, `efgh.sha256h2(abcd, wk)` | ARM SHA256H, SHA256H2 (four rounds; state ABCD / EFGH, A in lane 0) |
| `w0.sha256su0(w1)`, `t.sha256su1(w2, w3)` | ARM SHA256SU0, SHA256SU1 (W0 in lane 0) |
| `cdgh.sha256rnds2(abef, wk)` | x86 SHA256RNDS2 (two rounds; state ABEF / CDGH with A in lane 3; `wk`'s lanes 0 and 1 are the implicit `xmm0`) |
| `a.sha256msg1(b)`, `a.sha256msg2(b)` | x86 SHA256MSG1, SHA256MSG2 (W0 in lane 0) |
| `abcd.sha1rnds4(ewk, func)` | x86 SHA1RNDS4 (four rounds, A in lane 3; `func` 0–3 is the immediate, a RangeError past 3) |
| `a.sha1nexte(b)`, `a.sha1msg1(b)`, `a.sha1msg2(b)` | x86 SHA1NEXTE, SHA1MSG1, SHA1MSG2 (W0 in lane 3, as the state) |
| `u32x4.hasShaArm()`, `u32x4.hasShaX86()` | whether the CPU has them (x86: SHA with SSSE3 and SSE4.1) |

Every method works everywhere: it is the instruction where the CPU has it, and otherwise an
exact emulation in Velt with the same result. The checks are false on another architecture, on
WebAssembly, in debug builds (Cranelift has no SHA instructions) and when `VELT_SIMD_DISABLE=sha`
is set (to test the emulation on a CPU that has them). In a loop of them the flag is read once,
before the loop, and tested in every iteration (a predictable branch per method); std's hashes
run their loops in functions compiled for the extension, where the check is known to be true
and there is no branch. With the shuffles above (`T.shuffle`, `reverseBytes`) these are what the
published SHA-1 / SHA-256 routines use:

```ts
import { u32x4 } from "velt:simd";

function main() {
  const abcd = u32x4.splat(0x6a09e667).withLane(1, 0xbb67ae85).withLane(2, 0x3c6ef372).withLane(3, 0xa54ff53a);
  const efgh = u32x4.splat(0x510e527f).withLane(1, 0x9b05688c).withLane(2, 0x1f83d9ab).withLane(3, 0x5be0cd19);
  const wk = u32x4.splat(0x428a2f98);
  // Four SHA-256 rounds: the instruction on arm64 release builds, the emulation elsewhere.
  const next = abcd.sha256h(efgh, wk);
  console.log(next.lane(0).toString(16)); // ab76ba6d
}
```

## Carry-less multiplication

`u64x2` has the CPU's carry-less multiplication: x86's PCLMULQDQ and ARM's PMULL, which
multiply two 64-bit lanes as polynomials over GF(2) (XOR instead of addition, so no carries)
into a 128-bit product, its low 64 bits in lane 0. It is what CRC folding (std's gzip CRC-32,
velt:compress) and GHASH are built on.

| Method | Instruction |
|---|---|
| `a.clmulLow(b)` | lane 0 of `a` times lane 0 of `b` (PCLMULQDQ imm `0x00`, PMULL) |
| `a.clmulHigh(b)` | lane 1 times lane 1 (imm `0x11`, PMULL2) |
| `a.clmulLowHigh(b)` | lane 0 of `a` times lane 1 of `b` (imm `0x10`) |
| `a.clmulHighLow(b)` | lane 1 of `a` times lane 0 of `b` (imm `0x01`) |
| `u64x2.hasClmulX86()`, `u64x2.hasClmulArm()`, `u64x2.hasClmul()` | whether the CPU has them (x86: PCLMULQDQ with SSSE3 and SSE4.1; `hasClmul`: either) |

As with the SHA instructions, every method works everywhere: the instruction where the CPU has
it, an exact emulation (a shift and XOR per set bit) elsewhere, on WebAssembly, in debug builds
and with `VELT_SIMD_DISABLE=clmul`. The emulation is far slower than the instruction, so code
with a table-driven alternative checks `u64x2.hasClmul()` first, as velt:compress's CRC-32 does
(about 0.45 instructions per byte with the instruction, 3.4 with its tables).

```ts
import { u64x2 } from "velt:simd";

function main() {
  // (x + 1)(x + 1) = x^2 + 1 over GF(2): 3 times 3 is 5 without carries.
  const p = u64x2.splat(3).clmulLow(u64x2.splat(3));
  console.log(p.lane(0), p.lane(1)); // 5 0
}
```

Generic code given a vector (`xs.includes(v)`, a `Map` key, `xs.join()`) compares and hashes
all 128 bits (so NaN equals NaN and `0` differs from `-0` there) and prints the type's name,
`[u8x16]`; `typeof v` is `"object"`. A vector cannot be the value of a module-level `const` yet.

A module that does not import a name from `velt:simd` can use it for a type of its own (as
TypeScript code written before the SIMD types may); one that imports it and declares it too gets
the usual "defined multiple times" error. `import { u8x16 as Bytes }` works as for any import.

```ts
import { f32x4, u8x16 } from "velt:simd";

// The dot product, sixteen floats per step in four sums.
function dot(a: f32[], b: f32[]): f32 {
  let s0 = f32x4.splat(0);
  let s1 = f32x4.splat(0);
  let s2 = f32x4.splat(0);
  let s3 = f32x4.splat(0);
  let i: usize = 0;
  while (f32x4.fits(a, i + 12) && f32x4.fits(b, i + 12)) {
    s0 = s0.add(f32x4.load(a, i).mul(f32x4.load(b, i)));
    s1 = s1.add(f32x4.load(a, i + 4).mul(f32x4.load(b, i + 4)));
    s2 = s2.add(f32x4.load(a, i + 8).mul(f32x4.load(b, i + 8)));
    s3 = s3.add(f32x4.load(a, i + 12).mul(f32x4.load(b, i + 12)));
    i += 16;
  }
  let s = s0.add(s1).add(s2.add(s3)).sum();
  while (i < a.length && i < b.length) {
    s += a[i] * b[i];
    i++;
  }
  return s;
}

// ASCII letters to lower case, in place.
function lower(d: u8[]) {
  let i: usize = 0;
  while (u8x16.fits(d, i)) {
    const v = u8x16.load(d, i);
    const upper = v.ge(u8x16.splat(65)).and(v.le(u8x16.splat(90)));
    u8x16.select(upper, v.add(u8x16.splat(32)), v).store(d, i);
    i += 16;
  }
  while (i < d.length) {
    if (d[i] >= 65 && d[i] <= 90) {
      d[i] = d[i] + 32;
    }
    i++;
  }
}

function main() {
  const a: f32[] = [];
  const b: f32[] = [];
  for (let k = 0; k < 20; k++) {
    a.push(k as f32);
    b.push(2);
  }
  console.log(dot(a, b)); // 380
  const text: u8[] = [72, 101, 76, 76, 79, 32, 87, 79, 82, 76, 68, 33, 32, 65, 66, 67, 68];
  lower(text);
  console.log(text[0], text[16]); // 104 100
}
```

Write the loop as above: a step while a whole vector fits (`T.fits(xs, i)`; with several
vectors per step, the last one's position). `fits` is the check `load` and `store` make, so in
such a loop the compiler removes theirs; a condition written by hand (`i + 16 <= d.length`)
works too, with the check left in.

The last elements, fewer than a vector, can be done one at a time as in `dot` above, or as one
more vector with `loadPartial` and `storePartial`, which is shorter: each reads or writes
only the elements that exist, in two overlapping scalar accesses (three single bytes for fewer
than four bytes). From about four elements left that is faster than a loop over them (on
x86-64, a count and an XOR of a 15-byte array took 0.4 times the instructions); for one to
three, the loop is (about half the instructions for one byte; `bench/simd/partial.sh`):

```ts
import { mask8x16, u8x16 } from "velt:simd";

// The number of bytes equal to `b`.
function count(d: u8[], b: u8): usize {
  const needle = u8x16.splat(b);
  let n: usize = 0;
  let i: usize = 0;
  while (u8x16.fits(d, i)) {
    n += u8x16.load(d, i).eq(needle).count();
    i += 16;
  }
  // The lanes past the end are 0, which `b` may be too: only the ones that exist count.
  const rest = mask8x16.prefix((d.length as usize) - i);
  return n + u8x16.loadPartial(d, i).eq(needle).and(rest).count();
}

function main() {
  const d: u8[] = Buffer.alloc(37);
  d[3] = 9;
  d[36] = 9;
  console.log(count(d, 9), count(d, 0)); // 2 35
}
```

`bench/simd` measures these kernels against plain loops and against Rust.
