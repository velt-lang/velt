# Fuzz regressions

Minimized inputs for findings that a golden can't express (hangs, timeouts). Replay one with
`cargo +nightly fuzz run <target> regressions/<target>/<file> -- -timeout=5`; each must pass.

| File | Target | Finding |
|---|---|---|
| `parse/exponential_type_parens.velt` | `parse` | parse time that doubles with every nested `(` in a type; covered by `nested_parentheses` in `crates/velt_syntax/src/linear_tests.rs` |
