# Fuzz regressions

Minimized inputs for findings that a golden can't express (hangs, timeouts). Replay one with
`cargo +nightly fuzz run <target> regressions/<target>/<file> -- -timeout=5`; each should pass
once its bug is fixed.

| File | Target | Finding |
|---|---|---|
| `parse/exponential_type_parens.vlt` | `parse` | parse time doubles with every nested `(` in a type (26 levels ≈ 15 s) |
