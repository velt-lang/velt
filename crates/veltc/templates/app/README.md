# {{name}}

A Velt application.

```sh
velt run                 # build and run src/main.vlt
velt run -- Ada          # program arguments go after `--`
velt dev                 # rebuild and restart on every change
velt test                # run tests/*.test.vlt
velt fmt                 # format the sources
velt build --release     # optimized binary: target/velt/{{name}}
```

| File | What |
|---|---|
| `src/main.vlt` | entry point |
| `src/greet.vlt` | the greeting (imported by main and the tests) |
| `tests/greet.test.vlt` | tests: every exported `test_*` function |
