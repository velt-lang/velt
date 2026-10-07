# Struct benchmarks (#641)

Measures what a guaranteed-inline, immutable `struct` value type would win. Each workload is
written several ways, all printing the same output:

| file | what it is |
|---|---|
| `<name>_struct.vlt` | today's `struct`, used as a value: new values from small methods, whole elements replaced |
| `<name>_mut.vlt` | today's `struct` updated field by field (`bs[i].vel = …`, `p.y = -p.y`): allowed today, an error under the value-type design |
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

RESULTS_PLACEHOLDER
