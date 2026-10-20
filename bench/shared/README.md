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

`RwLock` / `Mutex` throughput, median of 3 interleaved runs (M-series Mac, 4+6 cores):

| reads | tasks | `lock_mix` | `lock_mix_tiny` |
|---:|---:|---:|---:|
| 0% | 1 / 4 / 10 | 1.05 / 1.02 / 0.97 | 1.07 / 0.99 / 1.07 |
| 50% | 1 / 4 / 10 | 1.04 / 0.98 / 0.89 | 0.91 / 0.92 / 0.88 |
| 90% | 1 / 4 / 10 | 1.05 / 0.98 / 0.97 | 0.96 / 0.87 / 0.80 |
| 99% | 1 / 4 / 10 | 1.18 / 1.04 / 1.13 | 1.02 / 0.91 / 0.76 |
| 100% | 1 / 4 / 10 | 1.16 / 6.2 / 15.7 | 1.06 / 7.2 / 13.4 |

While writes come often the lock is revoked and readers take its mutex, so the two cost about
the same. Where `lock_mix_tiny` still trails, two things the lock itself does not cost show:
`Mutex`'s read and write go through one `with`, which the compiler merges into one call with
the branch inside the lock (an `RwLock` whose reads also go through `write` ties: 142 against
140 M/s at 90% and 10 tasks), and the check of the lock's mode before taking the mutex, which a
callback of one load and ten threads taking turns magnify to about 15%.
