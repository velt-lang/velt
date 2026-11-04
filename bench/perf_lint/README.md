# The `perf/…` lint rules' measurements

Each rule of `velt check --perf` (docs/internals/design/lint-profiles.md, "Rules") comes with a
pair of programs here: the TypeScript form the rule finds (`*_ts.vlt`) and the Velt form its fix
writes (`*_velt.vlt`), same output. Instructions retired, release builds with the release
runtime, Linux x86-64 (2026-11-03):

| Rule | TypeScript form | Velt form |
|---|---:|---:|
| `perf/dynamic-record` | 1,894,239,917 | 7,244,250 |
| `perf/class-as-data` | 258,905,293 | 78,707,235 |
| `perf/foreach-capture` | 111,321,936 | 33,186,825 |
| `perf/number-double` | 1,101,048,035 | 621,048,026 |

To measure one (a release `velt`, or `VELT_RT_LIB` pointing at a release `libvelt_rt.a`):

```sh
velt build --release bench/perf_lint/foreach_capture_ts.vlt -o /tmp/ts
valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null /tmp/ts
```

Measured and left without a rule, because the TypeScript form is as fast: `JSON.parse(s) as T`
against `JSON.parse<T>(s)`, string appends in a loop (`s += x`, templates, a field), `forEach`
whose callback assigns nothing outside it, and `reduce`.
