# Compile-time benchmark (Windows / PowerShell 7): `velt build --release --emit vir` (load + parse,
# sema, lowering to VIR, verification, the optimizer) on a few programs, best of -Runs runs per
# stage as `velt build -v` reports it. Programs: examples/http_hello.vlt (std/http + the
# prelude), all_std.vlt (every std module), two generated ones of -Units units from unit.tmpl
# (a class hierarchy, an interface impl and functions modifying an array param per unit):
# "units" with call chains of 8, "chain" with one call chain through the whole program (worst
# case for modification inference), and "long_main": 4 × -Units classes with an override and a
# generic instance each, all used from one `main` (worst case for per-function analyses).
# A second table times `velt check` (parse + sema only, and the whole command) and the link of a
# debug build: against the shared runtime (the default), the static one (VELT_RT_LINK=static),
# and a rebuild with nothing changed (the link is skipped).
#
#   pwsh bench/compile/run.ps1 [-Runs 10] [-Units 1000] [-Velt <path to velt>] [-LinkOnly]
#
# -LinkOnly prints only the second table (check and link times), skipping the front-end runs
# (which need several GB of memory for long_main).
#
# Without -Velt it builds velt (release) first. For an older velt, set $env:VELT_STD to that
# version's std. `velt build --timings` breaks the optimizer down by pass.
param(
    [int]$Runs = 10,
    [int]$Units = 1000,
    [string]$Velt = "",
    [switch]$LinkOnly
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$out = Join-Path $target "bench/compile"
New-Item -ItemType Directory -Force $out | Out-Null

if (-not $Velt) {
    Write-Host "building velt and the runtimes (release)..."
    cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path (Join-Path $root "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $Velt = Join-Path $target "release/velt.exe"
}

# A program of $n units; unit i's g calls unit i-1's except every $chain-th unit (see unit.tmpl).
function New-Program([int]$n, [int]$chain) {
    $unit = (Get-Content -Raw (Join-Path $PSScriptRoot "unit.tmpl")) -replace "`r`n", "`n"
    $sb = [Text.StringBuilder]::new("interface Scorer {`n  score(x: i64): i64;`n}`n`n")
    for ($i = 0; $i -lt $n; $i++) {
        $link = if ($i % $chain -eq 0) { "  xs.push(1);`n  const r = 0;" }
                else { "  const r = g$($i - 1)(xs, new Base$($i - 1)(0));" }
        [void]$sb.Append($unit.Replace("@CHAIN@", $link).Replace("@I@", "$i"))
    }
    [void]$sb.Append("function main() {`n  const xs: i64[] = [];`n")
    for ($i = 0; $i -lt $n; $i++) { [void]$sb.Append("  f$i(new Sub$i($i, 1.0), xs, `"s`");`n") }
    [void]$sb.Append("}`n")
    return $sb.ToString()
}

# $n classes C_i with a subclass S_i overriding `area`, and a main that makes one of each,
# calls `area` through the base type, and passes it to a generic function (one instance each).
function New-LongMain([int]$n) {
    $sb = [Text.StringBuilder]::new("function count<T>(xs: T[]): i64 {`n  return xs.length as i64;`n}`n`n")
    for ($i = 0; $i -lt $n; $i++) {
        [void]$sb.Append("class C$i {`n  id: i64;`n  constructor(id: i64) {`n    this.id = id;`n  }`n  area(): i64 {`n    return this.id;`n  }`n}`n`n")
        [void]$sb.Append("class S$i extends C$i {`n  constructor(id: i64) {`n    super(id);`n  }`n  override area(): i64 {`n    return this.id + 1;`n  }`n}`n`n")
    }
    [void]$sb.Append("function main() {`n  let t = 0;`n")
    for ($i = 0; $i -lt $n; $i++) {
        [void]$sb.Append("  const b${i}: C$i = new S$i($i);`n  t += b$i.area() + count([b$i]);`n")
    }
    [void]$sb.Append("  console.log(t);`n}`n")
    return $sb.ToString()
}

$unitsFile = Join-Path $out "units_$Units.vlt"
$chainFile = Join-Path $out "chain_$Units.vlt"
$longFile = Join-Path $out "long_main_$($Units * 4).vlt"
[IO.File]::WriteAllText($unitsFile, (New-Program $Units 8))
[IO.File]::WriteAllText($chainFile, (New-Program $Units $Units))
[IO.File]::WriteAllText($longFile, (New-LongMain ($Units * 4)))
$programs = [ordered]@{
    "http_hello"     = Join-Path $root "examples/http_hello.vlt"
    "all_std"        = Join-Path $PSScriptRoot "all_std.vlt"
    "units_$Units"   = $unitsFile
    "chain_$Units"   = $chainFile
    "long_main_$($Units * 4)" = $longFile
}

$stages = @("parse", "sema", "lower", "verify", "optimize")
$front = @("parse", "sema", "lower")
if (-not $LinkOnly) {
Write-Output "| program | root file lines | load + parse | sema | lower | verify | optimize | front end |"
Write-Output "|---|---|---|---|---|---|---|---|"
foreach ($name in $programs.Keys) {
    $file = $programs[$name]
    $best = @{}
    for ($r = 0; $r -lt $Runs; $r++) {
        $log = & $Velt build -v --release $file --emit vir 2>&1 | Where-Object { $_ -is [Management.Automation.ErrorRecord] -or "$_" -match "^velt: " }
        if ($LASTEXITCODE -ne 0) { throw "velt build $file failed:`n$($log -join "`n")" }
        $sum = 0.0
        foreach ($line in $log) {
            if ("$line" -match "^velt: (\w+)\s+([\d.]+) ms") {
                $stage, $ms = $Matches[1], [double]$Matches[2]
                if ($stage -eq "total") { continue }
                if ($front -contains $stage) { $sum += $ms }
                if (-not $best.ContainsKey($stage) -or $ms -lt $best[$stage]) { $best[$stage] = $ms }
            }
        }
        if (-not $best.ContainsKey("front") -or $sum -lt $best["front"]) { $best["front"] = $sum }
    }
    $lines = @(Get-Content $file).Count
    $inv = [Globalization.CultureInfo]::InvariantCulture
    $cells = @($stages + "front" | ForEach-Object { $best[$_].ToString("F1", $inv) })
    Write-Output ("| $name | $lines | " + ($cells -join " | ") + " |")
}
Write-Output ""
Write-Output "Best of $Runs runs per stage, milliseconds (``velt build -v --release --emit vir``); front end = best run's parse + sema + lower."
}

# Stage times (`velt: <stage> <ms> ms` lines) of one velt run, plus its wall time as "wall".
function Measure-Velt([string[]]$VeltArgs) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $log = & $Velt @VeltArgs 2>&1 | Where-Object { $_ -is [Management.Automation.ErrorRecord] -or "$_" -match "^velt: " }
    $watch.Stop()
    if ($LASTEXITCODE -ne 0) { throw "velt $($VeltArgs -join ' ') failed:`n$($log -join "`n")" }
    $times = @{ "wall" = $watch.Elapsed.TotalMilliseconds }
    foreach ($line in $log) {
        if ("$line" -match "^velt: (\w+)\s+([\d.]+) ms") { $times[$Matches[1]] = [double]$Matches[2] }
    }
    return $times
}

# Link stage of a debug build of $file; without -Relink the link stamp is deleted first.
function Measure-DebugLink([string]$file, [string]$exe, [string]$link, [switch]$Relink) {
    if (-not $Relink) { Remove-Item -ErrorAction SilentlyContinue "$exe.exe.link-stamp" }
    $env:VELT_RT_LINK = $link
    try { return (Measure-Velt @("build", "-v", $file, "-o", $exe))["link"] }
    finally { Remove-Item Env:VELT_RT_LINK -ErrorAction SilentlyContinue }
}

Write-Output ""
Write-Output "| program | check: parse + sema | check: whole command | debug link: shared runtime | debug link: static runtime | rebuild, nothing changed: link |"
Write-Output "|---|---|---|---|---|---|"
$inv = [Globalization.CultureInfo]::InvariantCulture
foreach ($name in @("http_hello", "all_std", "units_$Units")) {
    $file = $programs[$name]
    $exe = Join-Path $out "${name}_debug"
    $check = @(); $wall = @(); $shared = @(); $static = @(); $relink = @()
    for ($r = 0; $r -lt $Runs; $r++) {
        $t = Measure-Velt @("check", "-v", $file)
        $check += $t["parse"] + $t["sema"]
        $wall += $t["wall"]
    }
    for ($r = 0; $r -lt $Runs; $r++) { $shared += Measure-DebugLink $file $exe "" }
    for ($r = 0; $r -lt $Runs; $r++) { $static += Measure-DebugLink $file $exe "static" }
    for ($r = 0; $r -lt $Runs; $r++) { $relink += Measure-DebugLink $file $exe "static" -Relink }
    $cells = @($check, $wall, $shared, $static, $relink | ForEach-Object { ($_ | Measure-Object -Minimum).Minimum.ToString("F1", $inv) })
    Write-Output ("| $name | " + ($cells -join " | ") + " |")
}
Write-Output ""
Write-Output "Best of $Runs runs, milliseconds: ``velt check -v`` (its parse + sema, and the wall time of the whole process) and the ``link`` stage of ``velt build -v`` (debug)."
