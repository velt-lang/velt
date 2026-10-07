# Struct benchmarks (#641)

Measures what a guaranteed-inline, immutable `struct` value type would win. Each workload is
written several ways, all printing the same output:

| file | what it is |
|---|---|
| `<name>_struct.vlt` | today's `struct`, used as a value: new values from small methods, whole elements replaced |
| `<name>_mut.vlt` | today's `struct` updated field by field (`bs[i].vel = …`, `p.y = -p.y`): allowed today, an error under the value-type design |
| `particles_shared.vlt` | `particles_mut.vlt` plus one second reference to an element: the struct type becomes a counted heap object |
| `<name>_class.vlt` | a `class` with `new` for every value, the usual TypeScript style (valid TypeScript) |
| `<name>_flat.vlt` | the objects flattened by hand into parallel `number[]` arrays or a packed number key: the code a guaranteed-inline value type should reach (valid TypeScript) |
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

Measured on 2026-10-07 at `main` 4992a14f (LLVM release, `VELT_THREADS=1`), WSL2 Ubuntu 20.04,
valgrind 3.15, rustc 1.99 `-O`, Node 24.16. Instruction counts are the measure; the
wall-clock table is from a shared machine under load and only indicative.

Instructions executed (millions):

| workload | struct | mut | shared | class | flat | Rust | struct / flat | class / flat | flat / Rust | struct / Rust |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| vecmath | 635 | 645 | – | 11,760 | 718 | 665 | 0.88 | 16.39 | 1.08 | 0.95 |
| particles | 102 | 77 | 89 | 1,069 | 71 | 102 | 1.43 | 15.01 | 0.70 | 1.00 |
| points | 150 | – | – | 689 | 174 | 152 | 0.86 | 3.96 | 1.14 | 0.99 |
| mapkey | 242 | – | – | 2,747 | 151 | 342 | 1.60 | 18.21 | 0.44 | 0.71 |

Heap blocks allocated:

| workload | struct | mut | shared | class | flat | Rust |
|---|---:|---:|---:|---:|---:|---:|
| vecmath | 1 | 1 | – | 95,000,036 | 7 | 9 |
| particles | 2 | 2 | 20,002 | 10,020,152 | 4 | 34 |
| points | 5 | – | – | 5,000,005 | 5 | 103 |
| mapkey | 21 | – | – | 1,000,021 | 21 | 25 |

Best wall-clock time of 3 interleaved runs (ms, including process start; Node runs the class
and flat files as TypeScript):

| workload | struct | mut | shared | class | flat | Rust | Node class | Node flat |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| vecmath | 128 | 160 | – | 1,366 | 137 | 94 | 773 | 238 |
| particles | 271 | 41 | 35 | 232 | 29 | 28 | 955 | 455 |
| points | 84 | – | – | 112 | 66 | 63 | 1,598 | 533 |
| mapkey | 330 | – | – | 893 | 321 | 295 | 959 | 399 |

What it shows:
- **Today's `struct`, used as a value** (no field writes, nothing shared) is already stored
  inline: no allocation, and within 0.71–1.00× of Rust's instructions. The value-type design
  turns this from a whole-program inference into a guarantee.
- **Classes**, the TypeScript way of writing the same values, execute 4–18× the instructions
  of the flat code and allocate one block per value (1M to 95M blocks; in mapkey, the string
  keys): `heap_sroa` removes allocations only where inlining puts an object's whole life in one
  function, never for objects stored in arrays or maps.
- **One shared value** (`particles_shared`) makes a struct type whose fields are assigned a
  counted object everywhere: every element becomes its own heap block (20,002 blocks) and the
  arrays hold pointers. Immutable structs cannot reach this state.
- Two gaps remain against the hand-flattened code, both work items for the implementation:
  replacing a whole element (`pos[i] = p`, 1.43× flat; the field-by-field `mut` version is
  1.08×), and hashing and comparing a struct `Map` key (1.60× a packed number key, though
  below Rust's SipHash).
