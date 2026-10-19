| server | route | conns | req/s | p50 | p99 | non-2xx | peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| velt | noop | 16 | 38043.92 | 391.00us | 1.45ms | 0 | 15 MB |
| velt | noop | 64 | 40061.88 | 1.40ms | 3.48ms | 0 | 17 MB |
| velt | noop | 256 | 27195.81 | 8.66ms | 16.70ms | 0 | 22 MB |
| velt | increment | 16 | 32047.67 | 453.00us | 1.90ms | 0 | 22 MB |
| velt | increment | 64 | 35622.23 | 1.62ms | 3.82ms | 0 | 22 MB |
| velt | increment | 256 | 24549.60 | 9.68ms | 19.13ms | 0 | 27 MB |
| node | noop | 16 | 2259.67 | 5.82ms | 44.52ms | 0 | 193 MB |
| node | noop | 64 | 2563.83 | 21.53ms | 359.88ms | 0 | 224 MB |
| node | noop | 256 | 2584.02 | 56.36ms | 655.05ms | 0 | 225 MB |
| node | increment | 16 | 2416.57 | 5.37ms | 44.43ms | 0 | 259 MB |
| node | increment | 64 | 2375.02 | 23.78ms | 315.03ms | 0 | 276 MB |
| node | increment | 256 | 2185.06 | 61.21ms | 801.58ms | 0 | 280 MB |
| node-bare | noop | 16 | 11670.57 | 1.26ms | 6.51ms | 0 | 68 MB |
| node-bare | noop | 64 | 12380.31 | 4.66ms | 9.64ms | 0 | 77 MB |
| node-bare | noop | 256 | 12426.84 | 18.97ms | 1.02s | 0 | 95 MB |
