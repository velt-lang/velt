| server | route | conns | req/s | p50 | p99 | non-2xx | peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|
| velt | noop | 16 | 45415.04 | 272.00us | 2.14ms | 0 | 21 MB |
| velt | noop | 64 | 61960.21 | 803.00us | 3.28ms | 0 | 21 MB |
| velt | noop | 256 | 65789.52 | 2.93ms | 8.88ms | 0 | 28 MB |
| velt | increment | 16 | 43773.70 | 283.00us | 2.10ms | 0 | 34 MB |
| velt | increment | 64 | 57321.11 | 0.87ms | 3.45ms | 0 | 34 MB |
| velt | increment | 256 | 62118.72 | 3.21ms | 9.60ms | 0 | 34 MB |
| node | noop | 16 | 3036.33 | 4.58ms | 13.86ms | 0 | 218 MB |
| node | noop | 64 | 2961.03 | 20.02ms | 262.12ms | 0 | 243 MB |
| node | noop | 256 | 2881.98 | 57.37ms | 743.55ms | 0 | 253 MB |
| node | increment | 16 | 2510.80 | 5.47ms | 20.95ms | 0 | 265 MB |
| node | increment | 64 | 2569.74 | 23.19ms | 277.39ms | 0 | 301 MB |
| node | increment | 256 | 2373.05 | 63.42ms | 743.22ms | 0 | 305 MB |
| node-bare | noop | 16 | 12914.21 | 1.05ms | 3.65ms | 0 | 69 MB |
| node-bare | noop | 64 | 12724.19 | 4.54ms | 10.50ms | 0 | 78 MB |
| node-bare | noop | 256 | 12486.27 | 18.66ms | 1.08s | 0 | 96 MB |
