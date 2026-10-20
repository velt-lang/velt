# `shared<T>` reads

`store_reads.vlt`: 8 tasks read one `shared<Store>` (200 products, a `Map`, counted classes)
through ordinary functions, 300 rounds each. Every task must print the same total.

```sh
velt build --release bench/shared/store_reads.vlt -o /tmp/store_reads
/usr/bin/time -l /tmp/store_reads   # macOS: peak RSS and instructions retired
```

Instructions retired (M-series Mac, release runtime, 3 runs each, identical output):

| compiler | instructions |
|---|---:|
| each call gets a copy of the store (#36 first step) | 0.92 G |
| frozen instances: calls borrow the store (#708) | 0.04 G |

## `RwLock` against `Mutex`

`lock_mix.vlt` (a `Map` lookup per operation) and `lock_mix_tiny.vlt` (one field per
operation): tasks share one lock and read or write it in a fixed mix.

```sh
velt build --release bench/shared/lock_mix.vlt -o /tmp/lock_mix
/tmp/lock_mix rwlock 10 90   # 10 tasks, 90 reads per 100 operations: M operations/s
```

`RwLock` / `Mutex` throughput, median of 5 interleaved runs (M-series Mac, 4+6 cores):

| reads | tasks | `lock_mix` | `lock_mix_tiny` |
|---:|---:|---:|---:|
| 0% | 1 / 4 / 10 | 1.11 / 1.96 / 2.34 | 1.27 / 2.38 / 1.94 |
| 50% | 1 / 4 / 10 | 1.21 / 2.11 / 1.86 | 1.22 / 2.42 / 1.28 |
| 90% | 1 / 4 / 10 | 1.23 / 2.17 / 1.52 | 1.16 / 2.38 / 1.17 |
| 99% | 1 / 4 / 10 | 1.24 / 2.21 / 1.55 | 1.13 / 2.43 / 1.23 |
| 100% | 1 / 4 / 10 | 1.19 / 7.4 / 17.9 | 1.30 / 11.8 / 15.0 |

Where `RwLock` gets ahead while writes come often: its common reads and writes skip the
thread-local lookup a `Mutex` makes on each lock and unlock (generated code hands the reader
state from one lock call to the next), and its waiters back off between looks at the lock, so
the holder keeps the lock's cache line and short callbacks run several in a row. Two runs of
`lock_mix_tiny` at 10 tasks at once (20 busy threads on 10 cores) stay ahead too (×1.1–1.7).
