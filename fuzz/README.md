# Fuzzing (cargo-fuzz / libFuzzer)

Standalone crate (its own `[workspace]`; not part of the compiler workspace or its gates).
Needs a nightly toolchain and `cargo install cargo-fuzz`; on Windows use WSL or Linux.

| Target | Input | Property |
|---|---|---|
| `parse` | source text | the parser never panics; a file that parses formats (`velt fmt`) to text that parses to the same AST (spans aside), and formatting is idempotent |
| `compile` | source text | load + parse + sema + lowering + VIR verification never panic and never report an ICE |
| `opt` | seed + 2 args | a random VIR program (the LLVM backend tests' generator) gives the same result and extern calls in the reference interpreter before and after `velt_opt` (`None` and `Speed`), and stays valid |
| `codegen` | seed | the same programs, plain and optimized, compile with Cranelift (plain and optimizing) and to LLVM IR |
| `json` | bytes | the runtime JSON reader never panics; `stringify(parse(x))` parses back and is a fixpoint |
| `float_fmt` | 8 bytes (f64 bits) | the runtime's number formatting equals ECMAScript `Number::toString` (reference in `src/numbers.rs`) and round-trips |

```sh
cd fuzz
./seed.sh                                    # seed corpora from every .vlt file in the repo
cargo +nightly fuzz run parse -- -max_total_time=600
cargo +nightly fuzz run compile -s none      # no sanitizer: ~20x faster for this target
cargo +nightly test --lib                    # the properties on known inputs
```

The properties live in `src/` (one module per stage) so they are unit-tested; the targets in
`fuzz_targets/` are one-liners. `velt_rt`'s rlib defines the C `main`, so the fuzz binaries
start libFuzzer from the `velt_main` it calls (`src/entry.rs`).
