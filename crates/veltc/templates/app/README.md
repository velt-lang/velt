# {{name}}

A Velt application.

```sh
velt run                 # build and run src/main.velt
velt run -- Ada          # program arguments go after `--`
velt dev                 # rebuild and restart on every change
velt test                # run tests/*.test.velt
velt fmt                 # format the sources
velt build --release     # optimized binary: target/velt/{{name}}
```

| File | What |
|---|---|
| `src/main.velt` | entry point |
| `src/greet.velt` | the greeting (imported by main and the tests) |
| `tests/greet.test.velt` | tests: every exported `test_*` function |
