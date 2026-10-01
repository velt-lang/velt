# reverse-complement

> The timing tables below were measured while other agents shared the machine. The authoritative
> numbers are the serial run in `../RESULTS.md`.

Rules: <https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/revcomp.html>.
Stdin is the output of `fasta 25000000` (official size); quick stdin is `fasta 1000` = the
official 10 KB `revcomp-input.txt`, and `expected-quick.txt` is the official
`revcomp-output.txt`.

## Sources

| File | Program | Authors | Threads |
|---|---|---|---|
| `rust/src/bin/reverse-complement.rs` | [Rust #1](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-rust-1.html) (fastest) | Ryohei Machida (inspired by C++ #2, Adam Kewley) | rayon |
| `rust/src/bin/reverse-complement_st.rs` | Rust #1 on a one-thread rayon pool | | 1 |
| `main.go` | [Go #6](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-go-6.html) (fastest Go) | Dirk Moerenhout | all cores |
| `main_st.go` | [Go #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-go-2.html) (fastest single-threaded Go) | K P anonymous, Andrew Martin | 1 |
| `main.js` | [Node #2](https://benchmarksgame-team.pages.debian.net/benchmarksgame/program/revcomp-node-2.html) (fastest Node with correct output) | Joe Farro, Jos Hirth, 10iii | 1 |
| `main.vlt` | idiomatic Velt: `openRead("/dev/stdin")` byte chunks (like Node's `stdin.read()` Buffers), per-sequence `u8[]`, a 256-entry complement table, `openWrite("/dev/stdout")` | | 1 |
| `main_mt.vlt` | `main.vlt` with each sequence's output in 16384-line blocks, each block a spawned task over its own slice of the sequence | | all cores |

Deviations: Rust #1 uses the `memchr` crate; the bench project only depends on rayon and regex,
so `memchr(b, s)` is a local `s.iter().position(|&x| x == b)`. On aarch64 its SSSE3 path is
compiled out and the scalar `reverse_chunks` fallback is used (as on the official ARM runs).

Correctness: every implementation matches the official output at 1000 and the Rust output at
25,000,000 (Velt: both backends, both variants).

## Timings (Apple M4, 10 cores; best of 2 sessions × 5 runs; stdout to a file)

The machine was shared with other agents during both sessions (load average 12–35), so wall
times are inflated and noisy; CPU seconds are the more reliable column. Versions: velt 0.1.0 (8f35554), rustc 1.98.1 (`-C target-cpu=native`, LTO), go 1.27.1, node 24.11.1, bun 1.4.2.

| Implementation | wall s | CPU s | RSS MB |
|---|---:|---:|---:|
| Rust reverse-complement (#1, rayon) | 0.260 | 0.297 | 125.1 |
| Rust reverse-complement_st | 0.273 | 0.231 | 124.9 |
| Velt LLVM main | 1.098 | 1.037 | 410.5 |
| Velt Cranelift main | 1.257 | 1.203 | 414.1 |
| Velt LLVM main_mt | 1.365 | 1.623 | 472.8 |
| Velt Cranelift main_mt | 1.151 | 1.709 | 443.6 |
| Go main (#6, parallel) | 0.255 | 0.363 | 143.0 |
| Go main_st (#2) | 0.452 | 0.395 | 161.0 |
| Node main (#2) | 4.693 | 4.491 | 225.1 |
| Bun main (same source) | 3.940 | 3.767 | 330.7 |

`main_mt.vlt` is not faster: the time goes into the sequential per-byte parsing loop in `main`
(below), and the tasks add a copy of every slice.

## Gap: Velt main vs Rust (~4× wall, ~4.5× CPU, 3.3× memory)

`sample` of `main.vlt`: the per-byte loop in `main$poll` has ~4× the samples of
`reverseComplement`.

### 1. Locals of an `async` function live in the state-machine frame (lowering / velt_opt)
The chunk loop (`for (const b of chunk) { … seq.push(b) … }`) runs inside `async main`, so
`seq`, `header` and `inHeader` are fields of the future's frame, and every byte loads and stores
them through the frame pointer — the array length is stored and reloaded per push, a
store→load round trip on the loop-carried dependency:

```asm
ldp   x23, x8, [x19, #0x58]   ; seq.length, seq.capacity   (x19 = frame)
cmp   x23, x8
ldr   x0, [x19, #0x50]        ; seq.data
strb  w28, [x0, x23]
add   x8, x23, #0x1
str   x8, [x19, #0x58]        ; seq.length
```

The same loop in a sync function keeps `ptr/len/cap` in registers (`strb w28,[x21,x24]; add x24,
x24,#1`). Measured on the input (filter out `\n`, push the rest):

| | CPU s |
|---|---:|
| Velt, loop in `async main` (chunks from `openRead`) | 0.73 total |
| Velt, same loop in a sync `main` (after `readAllSync`) | 0.35 for the loop |
| Rust, same byte loop over 64 KB `read`s | 0.24 total |

Moving the loop into a sync helper does not help, because LLVM inlines it back into the poll
function. The poll functions are emitted without attributes (`define internal i32
@"_V4main$poll"(ptr %p0, ptr %p1)`), and the loop contains calls (`velt_rt_realloc` on the grow
path, child polls), so LLVM cannot promote the frame fields. **Proposed:**
- mark the frame parameter of `$poll` functions `noalias nonnull dereferenceable(frame size)`:
  the executor owns the frame exclusively during a poll;
- in `velt_opt`, promote frame slots to SSA temporaries inside await-free regions (load on entry
  to a loop that contains no suspension point, store back on exit and before calls that receive
  the frame), so a hot loop in an async function compiles like one in a sync function.

**Update:** the `velt_opt` half is done (`velt_opt::frame_slots`, FINDINGS 8.2): the byte loop
keeps `seq`'s fields and the flags in registers. On the Windows machine the whole program went
from 1.27 to 1.06 CPU s (bench/RESULTS.md "Backend round"); not yet re-measured here.

### 2. Byte-at-a-time processing: std has no bulk `u8[]` operations (std)
Rust #1 and Go #2 find line ends with `memchr` and copy whole lines (`extend_from_slice`/`copy`),
then reverse in place from both ends. A Velt program can only `push` one byte at a time (no
`indexOf(byte, from)`, no `TypedArray.set` / `copyWithin` / `subarray`, no `push(...xs)` for
arrays). **Proposed std additions on `u8[]` (Node `Buffer` / `Uint8Array` names):**
`indexOf(value, from)` (memchr), `set(source, offset)` / `pushRange(source, start, end)`
(memcpy), `copyWithin`, and `new Uint8Array(n)` (zeroed allocation) — each a thin runtime call.

### 3. Memory: 410 MB vs 125 MB
`seq` grows by doubling (capacity up to 2× its 125 MB length), then `reverseComplement` builds a
second array of the same size (also grown by push). Rust #1 reverses in place in its read buffer. With the bulk
operations above (and a sized allocation for the output) the port could reverse in place too.

## Friction for a TypeScript developer
- A class field needs a type annotation even with an initializer (`inHeader = false;` is a parse
  error): filed as `tests/golden/bugs/parse_class_field_inferred_type.vlt`.
- No `Buffer`/`Uint8Array` API (see gap 2); byte buffers are `u8[]` grown by `push`.
- Async parameters are owned: passing the sequence to an async helper (`main_mt.vlt`) copies or
  moves it; a slice for a task (`seq.slice(a, b)`) is always a copy.

## Std and runtime round
`main.vlt` was rewritten on the new std building blocks (FINDINGS §8.6):
- stdin is read whole with `readAllBytesSync`.
- Headers are found with `indexOf` (memchr).
- The output goes into one `Buffer.alloc` array, sent with a single `stdout.write`.

CPU fell from 4.66× to 1.54× Rust. Peak RSS is higher (581 MB) because input and output are both
held; Rust reverses in place. `main_mt.vlt` only switched its output to `stdout.write`.
