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
