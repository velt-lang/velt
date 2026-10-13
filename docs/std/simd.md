# SIMD

Methods of the 128-bit vector types ([SIMD vectors](../reference/types.md#simd-vectors)), which
are in scope without an import (`std/prelude/global/simd_*.vlt`). `T` is a vector type
(`i8x16 u8x16 i16x8 u16x8 i32x4 u32x4 i64x2 u64x2 f32x4 f64x2`), `L` the type of its lanes, `n`
their number and `M` its mask type (`mask8x16`, `mask16x8`, `mask32x4`, `mask64x2`: lanes as
wide as the vector's). Each method is one instruction, or a few, on x86-64 (SSE2) and arm64
(NEON) and WebAssembly (`simd128`, which a program that uses vectors then needs from its
engine), in debug builds too.

| Member | Meaning |
|---|---|
| `T.splat(x)` | every lane `x` |
| `T.fits(xs, at = 0)` | whether the `n` elements of the `L[]` from `at` are inside it: what `load` and `store` check |
| `T.load(xs, at = 0)` | the `n` elements of the `L[]` from `at`; a RangeError panic unless all are inside the array |
| `v.store(xs, at = 0)` | writes the lanes to the `L[]` from `at`; a RangeError panic unless they fit (the array does not grow) |
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
| `toF32x4()` | `i32x4`, `u32x4`: each lane as the nearest `f32` |
| `toI32x4()` `toU32x4()` | `f32x4`: each lane truncated toward zero and clamped to the range, NaN as 0 |
| `v.asU8x16()`, … | the same 128 bits as another vector type, nothing converted: to and from `u8x16`, and between the types with as many lanes |

A mask `m`:

| Member | Meaning |
|---|---|
| `m.any()`, `m.all()` | whether some lane, every lane is set |
| `m.first()` | the index of the first set lane, `n` when none is |
| `m.count()` | the number of set lanes |
| `m.bitmask()` | a `u64` whose bit `i` is lane `i` |
| `and` `or` `xor` `not` | lane by lane |

The methods keep the types they declare: a lane of a `u8x16` is a `u8`, `first()` a `usize`.
Generic code given a vector (`xs.includes(v)`, a `Map` key, `xs.join()`) compares and hashes
all 128 bits and prints the type's name, `[u8x16]`.

```ts
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
vectors per step, the last one's position), then the rest one element at a time. `fits` is the
check `load` and `store` make, so in such a loop the compiler removes theirs. A condition
written by hand (`i + 16 <= d.length`) works too, with the check left in.
`bench/simd` measures these kernels against plain loops and against Rust.
