# Builds the compiler and the differential tester, then runs `difftest` with the given arguments.
#   pwsh tests/difftest/run.ps1 run tests/difftest/corpus
#   pwsh tests/difftest/run.ps1 fuzz --seeds 1..500
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
cargo build --quiet --manifest-path (Join-Path $here "../../Cargo.toml") -p veltc
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --quiet --release --manifest-path (Join-Path $here "Cargo.toml")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& (Join-Path $here "target/release/difftest") @args
exit $LASTEXITCODE
