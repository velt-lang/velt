# Struct benchmarks (#641)

Measures what the immutable `struct` value type wins. Each workload is written several ways,
all printing the same output (`ab/` holds more programs, each against Rust alone: see below):

| file | what it is |
|---|---|
| `<name>_struct.velt` | a `struct`: new values from small methods, whole elements replaced |
| `<name>_mut.velt` | an object type updated field by field (`bs[i].vel = …`, `p.y = -p.y`), which an immutable `struct` does not allow |
| `particles_shared.velt` | `particles_mut.velt` plus one second reference to an element: the object type becomes a counted heap object |
| `<name>_class.velt` | a `class` with `new` for every value, the usual TypeScript style (valid TypeScript) |
| `<name>_flat.velt` | the objects flattened by hand into parallel `number[]` arrays or a packed number key: the code a guaranteed-inline value type should reach (valid TypeScript) |
| `rust/<name>.rs` | Rust with a `#[derive(Clone, Copy)]` struct |

Workloads:
- **vecmath**: n-body style `Vec3` math (sub, dot, scale, add) over a `Body[]` of 5 bodies,
  1M steps.
- **particles**: 10,000 particles in `Vec2[]` position and velocity arrays, 500 steps of gravity
  and a bounce.
- **points**: build an array of 1M `{ x, y }` points with `push` and sum it, 5 times.
- **mapkey**: a 1M-step random walk counting visits per grid cell in a `Map` keyed by a
  two-number struct. The class version keys by the string `"x,y"` (class instances are
  identity keys, in TypeScript and in Velt); the flat version by `x * 65536 + y`.

Run (Linux, needs cargo, rustc, clang, valgrind, python3, and node 22.6+ for the Node columns):

```
bench/structs/run.sh                       # builds velt and the debug runtime first
bench/structs/run.sh --velt target/release/velt --rt-debug target/debug/libvelt_rt.a --only mapkey
```

It checks that every version prints the same output, then prints the instructions each one
executes (cachegrind, `VELT_THREADS=1`; counts repeat to within 0.1%) and the heap
allocations (Velt: `velt_rt_alloc` calls counted by the debug runtime linked into the same
optimized program, `VELT_RC_STATS=1`; Rust: valgrind DHAT).

## Results

Measured on 2026-11-05 (LLVM release, `VELT_THREADS=1`) in an Ubuntu 24.04 arm64 VM, valgrind
3.22, rustc 1.99 `-C opt-level=3`. Instruction counts are the measure (they repeat to within
0.1%); the wall-clock table is from a shared machine and only indicative.

Instructions executed (millions):

| workload | struct | mut | shared | class | flat | Rust | struct / flat | class / flat | flat / Rust | struct / Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| mapkey | 160 | – | – | 467 | 139 | 274 | 1.15 | 3.37 | 0.51 | 0.58 |
| particles | 49 | 91 | 93 | 1,063 | 54 | 48 | 0.91 | 19.83 | 1.11 | 1.00 |
| points | 106 | – | – | 641 | 131 | 112 | 0.81 | 4.88 | 1.18 | 0.95 |
| vecmath | 424 | 452 | – | 10,840 | 684 | 484 | 0.62 | 15.84 | 1.41 | 0.88 |

Heap blocks allocated:

| workload | struct | mut | shared | class | flat | Rust |
|---|---:|---:|---:|---:|---:|---:|
| mapkey | 22 | – | – | 22 | 22 | 27 |
| particles | 2 | 2 | 20,002 | 10,020,152 | 4 | 36 |
| points | 5 | – | – | 5,000,005 | 5 | 105 |
| vecmath | 1 | 1 | – | 95,000,016 | 7 | 11 |

Best wall-clock time of 3 interleaved runs (ms, including process start):

| workload | struct | mut | shared | class | flat | Rust |
|---|---:|---:|---:|---:|---:|---:|
| mapkey | 288 | – | – | 438 | 209 | 234 |
| particles | 7 | 6 | 27 | 191 | 5 | 9 |
| points | 21 | – | – | 59 | 21 | 17 |
| vecmath | 184 | 251 | – | 2,990 | 468 | 260 |

What it shows:
- **A `struct`** is stored inline (no allocation) and executes 0.58–1.00× Rust's instructions.
  Before #641 these programs got the same code, because the compiler proved each struct
  unshared; the value type makes that a guarantee that one second reference can no longer break
  (`particles_shared`: 20,002 heap blocks and pointer arrays).
- **Classes**, the TypeScript way of writing the same values, execute 3–20× the instructions of
  the flat code and allocate one block per value.
- **mapkey**: struct keys hash and compare by their bytes (a NaN key finds itself, `-0` and `0`
  are two keys), 1.15× a packed number key and well below Rust's SipHash.

### `ab/`: struct calls and arrays against Rust

`ab/` holds programs whose structs cross calls that are not inlined (`calls`: a recursive walk
passing and returning `Vec2`), are compared with `==` and kept in an array and a `Set` (`bfs`:
a grid search; before #641, comparing by identity made each `Cell` a counted heap object), or
are replaced whole in an array (`nbody`, the Benchmarks Game algorithm). `bench/structs/ab/run.sh` checks that each prints the same as its
Rust version and counts instructions (cachegrind on Linux, `/usr/bin/time -l` on macOS).

Instructions executed, same VM and date; base is `main` before #641:

| program | base | struct value type | Rust | value / base | value / Rust |
|---|---:|---:|---:|---:|---:|
| bfs | 1,825.6M | 930.7M | 1,189.9M | 0.51 | 0.78 |
| calls | 320.9M | 210.7M | 210.5M | 0.66 | 1.00 |
| nbody | 1,110.4M | 1,115.4M | 1,855.3M | 1.00 | 0.60 |

A struct of at most 16 bytes travels in registers: as its scalar fields when it has at most
four numbers or booleans (a `Vec2` in two float registers, as Rust passes it), otherwise as one
128-bit vector; results come back as one 128-bit vector.
