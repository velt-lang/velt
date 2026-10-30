"""The process benchmarks of the showcase: builds and benchmark definitions per category, all from
the existing suites under bench/ (see README.md for the list and the fairness rules).

Every `*_benchmarks(ctx)` function builds what its category needs and returns benchmark dicts
for engine.measure. A build that fails becomes an implementation with an `error` (shown as n/a
with the reason), never a missing row.
"""

import os
import shutil
import subprocess
import sys

MICRO = ["classes", "closures", "closures_borrowed", "closures_held", "closures_this", "fib",
         "floats", "hashmap", "int32", "loops", "nbody", "shapes", "sort", "strings"]
ASYNC = ["all_small_stored", "await_chain", "await_deep", "chan_item_size", "channel_pipeline",
         "fanout_all", "fanout_all_throwing", "hot_loop", "pingpong_pinned", "spawn_many",
         "timer_await", "timer_await_top", "timers", "top_level_await"]
JSON_PROGRAMS = ["edit", "navigate", "parse_typed", "parse_union", "parse_value", "stringify"]
CORPORA = ["canada", "citm_catalog", "twitter"]


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def sh(ctx, what, cmd, cwd=None, env=None):
    """Runs a build step; returns None on success, else the reason (also kept in ctx.failures)."""
    log(f"build: {what}")
    p = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})},
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if p.returncode == 0:
        return None
    tail = p.stdout.decode(errors="replace").strip().splitlines()[-6:]
    reason = f"build failed ({what}): {' | '.join(tail)}"
    ctx.failures.append(reason)
    log(reason)
    return reason


def velt_build(ctx, src, exe, extra=()):
    err = sh(ctx, f"velt {os.path.relpath(src, ctx.root)}",
             [ctx.velt, "build", "--release", *extra, src, "-o", exe])
    return err


def impl(label, lang, cmd, error=None, **kw):
    d = {"label": label, "lang": lang, "cmd": [str(c) for c in cmd], **kw}
    if error:
        d["error"] = error
    return d


def js_impls(ctx, path, args=(), label_suffix="", error=None, **kw):
    """Node and Bun running the same file."""
    out = []
    for lang, exe in (("node", ctx.node), ("bun", ctx.bun)):
        name = "Node" if lang == "node" else "Bun"
        err = error or (None if exe else f"{lang} not found")
        out.append(impl(f"{name}{label_suffix}", lang, [exe or lang, path, *args], err, **kw))
    return out


# Rust gets the build Velt's release build is: LLVM -O3, the whole program optimized as one
# unit (Velt compiles the program and the std code it uses into one module), and the same CPU
# level, x86-64-v2 (Velt's default on x86_64), or the machine's own CPU where a suite builds its
# Rust baseline for it and Velt gets `--cpu native` too.
RUST_CPU = "x86-64-v2"
RUSTC = ["rustc", "--edition", "2021", "-C", "opt-level=3", "-C", "codegen-units=1", "-C", "lto=fat"]


def rust_profile(native=False):
    """The environment every `cargo build --release` gets (overriding the suites' profiles)."""
    return {"CARGO_PROFILE_RELEASE_OPT_LEVEL": "3", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS": "1",
            "CARGO_PROFILE_RELEASE_LTO": "fat",
            "RUSTFLAGS": f"-C target-cpu={'native' if native else RUST_CPU}"}


def rustc(ctx, what, src, out):
    """A single-file Rust program, built as rust_profile builds a crate."""
    return sh(ctx, what, [*RUSTC, "-C", f"target-cpu={RUST_CPU}", "-o", out, src])


def cargo(ctx, what, manifest_dir, target, native=False):
    """`cargo build --release --locked`: the committed lockfile, the rust_profile build."""
    return sh(ctx, what, ["cargo", "build", "--release", "--locked", "-q", "--target-dir", target],
              cwd=manifest_dir, env=rust_profile(native))


# ---- CPU micro-benchmarks (bench/*.vlt, bench/rust, bench/node) ----

def micro_benchmarks(ctx):
    out = os.path.join(ctx.out, "micro")
    os.makedirs(out, exist_ok=True)
    benches = []
    for name in MICRO:
        if not ctx.wanted_name(name):
            continue
        rust = os.path.join(out, f"{name}-rust")
        rerr = rustc(ctx, f"rustc {name}", os.path.join(ctx.bench, "rust", f"{name}.rs"), rust)
        velt = os.path.join(out, f"{name}-velt")
        verr = velt_build(ctx, os.path.join(ctx.bench, f"{name}.vlt"), velt)
        benches.append({
            "category": "cpu", "name": name, "check": "ref",
            "impls": [impl("Rust", "rust", [rust], rerr), impl("Velt", "velt", [velt], verr),
                      *js_impls(ctx, os.path.join(ctx.bench, "node", f"{name}.js"))],
        })
    return benches


# ---- The Benchmarks Game (bench/benchmarks-game) ----

def read_conf(path):
    conf = {}
    for line in open(path):
        line = line.split("#", 1)[0].strip()
        if "=" in line:
            k, v = line.split("=", 1)
            conf[k.strip()] = v.strip()
    return conf


def game_benchmarks(ctx, category):
    """category "benchmarks-game": every program but regex-redux; "regex": regex-redux."""
    here = os.path.join(ctx.bench, "benchmarks-game")
    out = os.path.join(ctx.out, "game")
    os.makedirs(out, exist_ok=True)
    rust_target = os.path.join(out, "rust-target")
    if not hasattr(ctx, "game_rust_err"):
        ctx.game_rust_err = cargo(ctx, "benchmarks-game rust (target-cpu=native)",
                                  os.path.join(here, "rust"), rust_target, native=True)
    rust_bin = os.path.join(rust_target, "release")
    programs = sorted(d for d in os.listdir(here) if os.path.isfile(os.path.join(here, d, "bench.conf")))
    programs = [p for p in programs if (p == "regex-redux") == (category == "regex")]
    benches = []
    for program in programs:
        if not ctx.wanted_name(program):
            continue
        dirp = os.path.join(here, program)
        conf = read_conf(os.path.join(dirp, "bench.conf"))
        n = conf["QUICK_N"] if ctx.smoke else conf["N"]
        stdin, args = None, [n]
        if conf.get("STDIN") == "fasta":
            stdin, args = fasta_input(ctx, rust_bin, n), []
        check = f"file:{os.path.join(dirp, 'expected-quick.txt')}" if ctx.smoke else "ref"
        impls = []
        rust_files = sorted(f[:-3] for f in os.listdir(os.path.join(here, "rust", "src", "bin"))
                            if f == f"{program}.rs" or f.startswith(f"{program}_"))
        for b in rust_files:
            impls.append(impl(f"Rust {b}", "rust", [os.path.join(rust_bin, b), *args],
                              ctx.game_rust_err, stdin=stdin))
        for f in sorted(os.listdir(dirp)):
            stem, ext = os.path.splitext(f)
            if ext == ".vlt":
                exe = os.path.join(out, f"{program}-{stem}")
                err = velt_build(ctx, os.path.join(dirp, f), exe, ["--cpu", "native"])
                impls.append(impl(f"Velt {stem}", "velt", [exe, *args], err, stdin=stdin))
        for f in sorted(os.listdir(dirp)):
            stem, ext = os.path.splitext(f)
            if ext == ".js":
                impls += js_impls(ctx, os.path.join(dirp, f), args, f" {stem}", stdin=stdin)
        benches.append({"category": category, "name": f"{program} ({n if not stdin else 'fasta ' + n})",
                        "check": check, "impls": impls})
    return benches


def fasta_input(ctx, rust_bin, n):
    """The output of `fasta n` (the stdin of k-nucleotide, regex-redux, reverse-complement), from
    the Rust fasta, or Velt's when the Rust programs did not build (their outputs are the same)."""
    path = os.path.join(ctx.out, "game", f"fasta-{n}.txt")
    if os.path.isfile(path) and os.path.getsize(path) > 0:
        return path
    gen = os.path.join(rust_bin, "fasta")
    if not os.path.isfile(gen):
        gen = os.path.join(ctx.out, "game", "fasta-main")
        if not os.path.isfile(gen):
            velt_build(ctx, os.path.join(ctx.bench, "benchmarks-game", "fasta", "main.vlt"), gen, ["--cpu", "native"])
    with open(path, "wb") as f:
        try:
            subprocess.run([gen, n], stdout=f, check=False)
        except OSError as e:
            ctx.failures.append(f"fasta input {n}: {e}")
    return path


# ---- Async and concurrency (bench/async, bench/concurrent_map) ----

def async_benchmarks(ctx):
    here = os.path.join(ctx.bench, "async")
    out = os.path.join(ctx.out, "async")
    os.makedirs(out, exist_ok=True)
    target = os.path.join(out, "rust-target")
    rerr = cargo(ctx, "async tokio programs", os.path.join(here, "rust"), target)
    benches = []
    for name in ASYNC:
        if not ctx.wanted_name(name):
            continue
        rust = os.path.join(target, "release", name)
        velt = os.path.join(out, name)
        verr = velt_build(ctx, os.path.join(here, f"{name}.vlt"), velt)
        benches.append({
            "category": "async", "name": name, "check": "ref",
            "impls": [impl("Rust tokio multi-thread", "rust", [rust], rerr),
                      impl("Rust tokio current-thread", "rust", [rust, "current"], rerr),
                      impl("Velt", "velt", [velt], verr),
                      impl("Velt 1 thread", "velt", [velt], verr, env={"VELT_THREADS": "1"}),
                      *js_impls(ctx, os.path.join(here, "node", f"{name}.js"))],
        })
    if ctx.wanted_name("concurrent_map"):
        here = os.path.join(ctx.bench, "concurrent_map")
        velt = os.path.join(out, "concurrent_map")
        verr = velt_build(ctx, os.path.join(here, "map.vlt"), velt, ["--cpu", "native"])
        # rust/.cargo/config.toml builds for this CPU, as bbtree's own benchmark does.
        rtarget = os.path.join(out, "cmap-rust")
        rerr = cargo(ctx, "concurrent_map bbtree (git dependency)", os.path.join(here, "rust"), rtarget, native=True)
        benches.append({
            "category": "async", "name": "concurrent_map", "check": "cmap", "parse": "cmap",
            "unit": "Mops/s",
            "impls": [impl("Rust bbtree", "rust", [os.path.join(rtarget, "release", "concurrent_map_bench")], rerr),
                      impl("Velt ConcurrentMap", "velt", [velt], verr)],
        })
    return benches


# ---- JSON (bench/json, bench/json/corpora) ----

def json_benchmarks(ctx):
    out = os.path.join(ctx.out, "json")
    os.makedirs(out, exist_ok=True)
    benches = []
    here = os.path.join(ctx.bench, "json")
    target = os.path.join(out, "rust-target")
    rerr = cargo(ctx, "json serde_json programs", os.path.join(here, "rust"), target)
    for name in JSON_PROGRAMS:
        if not ctx.wanted_name(name):
            continue
        velt = os.path.join(out, name)
        verr = velt_build(ctx, os.path.join(here, f"{name}.vlt"), velt)
        benches.append({
            "category": "json", "name": name, "check": "ref",
            "impls": [impl("Rust serde_json", "rust", [os.path.join(target, "release", name)], rerr),
                      impl("Velt", "velt", [velt], verr),
                      *js_impls(ctx, os.path.join(here, "node", f"{name}.js"))],
        })
    corpora = os.path.join(here, "corpora")
    ferr = sh(ctx, "json corpora (download, checksums)", ["bash", os.path.join(corpora, "fetch.sh")])
    ctarget = os.path.join(out, "corpora-rust-target")
    cerr = ferr or cargo(ctx, "json corpora serde_json, sonic-rs, simd-json", os.path.join(corpora, "rust"), ctarget)
    rounds = "1" if ctx.smoke else "50"
    for op in ("value", "write"):
        velt = os.path.join(out, f"corpora-{op}")
        verr = ferr or velt_build(ctx, os.path.join(corpora, f"{op}.vlt"), velt)
        rust_check = "ref" if op == "value" else "json"
        for corpus in CORPORA:
            if not ctx.wanted_name(f"{corpus} {op}"):
                continue
            data = os.path.join(corpora, "data", f"{corpus}.json")
            args = [data, rounds]
            rb = os.path.join(ctarget, "release")
            benches.append({
                "category": "json", "name": f"{corpus}.json {'parse to a value' if op == 'value' else 'write a parsed value'} x{rounds}",
                "check": "ref",
                "input_bytes": os.path.getsize(data) if os.path.isfile(data) else None,
                "rounds": int(rounds),
                "impls": [*js_impls(ctx, os.path.join(corpora, "node", f"{op}.js"), args, error=ferr),
                          impl("Velt JSON.parseValue" if op == "value" else "Velt JsonValue.stringify()", "velt",
                               [velt, *args], verr),
                          impl("Rust serde_json", "rust", [os.path.join(rb, f"{op}_serde"), *args], cerr, check=rust_check),
                          impl("Rust sonic-rs", "rust", [os.path.join(rb, f"{op}_sonic"), *args], cerr, check=rust_check),
                          impl("Rust simd-json", "rust", [os.path.join(rb, f"{op}_simd"), *args], cerr, check=rust_check)],
            })
    # Typed: JSON.parse<T> into structs with the fields a program reads, and JSON.stringify of
    # the typed value (bench/showcase/json), against serde derive on the three Rust parsers. JS
    # has no typed decode: its row is JSON.parse with the same field reads.
    typed = os.path.join(ctx.here, "json")
    ttarget = os.path.join(out, "typed-rust-target")
    terr = ferr or cargo(ctx, "typed JSON (serde derive)", os.path.join(typed, "rust"), ttarget)
    tvelt = os.path.join(out, "typed")
    tverr = ferr or velt_build(ctx, os.path.join(typed, "typed.vlt"), tvelt)
    trust = os.path.join(ttarget, "release", "showcase_json_typed")
    for corpus, key in (("canada", "canada"), ("citm_catalog", "citm"), ("twitter", "twitter")):
        data = os.path.join(corpora, "data", f"{corpus}.json")
        size = os.path.getsize(data) if os.path.isfile(data) else None
        for op in ("parse", "write"):
            if not ctx.wanted_name(f"{corpus} typed"):
                continue
            rows = [impl("Velt JSON.parse<T>" if op == "parse" else "Velt JSON.stringify(typed)", "velt",
                         [tvelt, key, data, rounds, op], tverr)]
            for lib, label in (("serde", "serde_json"), ("sonic", "sonic-rs"), ("simd", "simd-json")):
                rows.append(impl(f"Rust {label} (serde derive)", "rust", [trust, lib, key, data, rounds, op], terr,
                                 check="json" if op == "write" else "ref"))
            if op == "parse":
                rows += js_impls(ctx, os.path.join(typed, "typed.mjs"), [key, data, rounds],
                                 " JSON.parse + field reads", error=ferr)
            else:
                rows += [impl("Node", "node", [], "n/a: the typed write is in the 'write a parsed value' row"),
                         impl("Bun", "bun", [], "n/a: the typed write is in the 'write a parsed value' row")]
            benches.append({"category": "json", "name": f"{corpus}.json typed {op} x{rounds}", "check": "ref",
                            "input_bytes": size, "rounds": int(rounds), "impls": rows})
    return benches


# ---- SQLite (bench/db sqlite, bench/sqlite) ----

def sqlite_benchmarks(ctx):
    out = os.path.join(ctx.out, "sqlite")
    os.makedirs(out, exist_ok=True)
    benches = []
    db = os.path.join(ctx.bench, "db")
    target = os.path.join(out, "db-rust-target")
    rerr = cargo(ctx, "db rusqlite", os.path.join(db, "rust"), target)
    velt = os.path.join(out, "db-sqlite")
    verr = velt_build(ctx, os.path.join(db, "velt", "sqlite.vlt"), velt)
    nerr = None
    if not os.path.isdir(os.path.join(db, "node", "node_modules")):
        nerr = sh(ctx, "npm ci (better-sqlite3)", [ctx.npm or "npm", "ci", "--no-audit", "--no-fund"],
                  cwd=os.path.join(db, "node"))
    quick = ["quick"] if ctx.smoke else []
    bun_file = os.path.join(ctx.here, "bun", "sqlite.ts")
    benches.append({
        "category": "sqlite", "name": "db workloads (file database, WAL)", "check": "results",
        "parse": "results", "unit": "ops/s",
        "impls": [impl("Rust rusqlite", "rust", [os.path.join(target, "release", "sqlite"), *quick], rerr),
                  impl("Velt sqlite package", "velt", [velt, *quick], verr),
                  impl("Node better-sqlite3", "node", [ctx.node or "node", os.path.join(db, "node", "sqlite.mjs"), *quick],
                       nerr or (None if ctx.node else "node not found")),
                  impl("Bun bun:sqlite", "bun", [ctx.bun or "bun", bun_file, *quick],
                       None if ctx.bun else "bun not found")],
    })
    rows = os.path.join(ctx.bench, "sqlite")
    rtarget = os.path.join(out, "rows-rust-target")
    rrerr = cargo(ctx, "sqlite rows rusqlite + serde_json", os.path.join(rows, "rust"), rtarget)
    rvelt = os.path.join(out, "rows")
    rverr = velt_build(ctx, os.path.join(rows, "rows.vlt"), rvelt)
    n = "200" if ctx.smoke else "20000"
    for q in ("feed", "post"):
        benches.append({
            "category": "sqlite", "name": f"rows {q} x{n} (in-memory, rows to JSON)", "check": "ref",
            "impls": [impl("Rust rusqlite + serde_json", "rust",
                           [os.path.join(rtarget, "release", "sqlite_rows_bench"), q, n], rrerr),
                      impl("Velt", "velt", [rvelt, q, n], rverr)],
        })
    return benches


# ---- Start-up (bench/showcase/programs/hello) ----

def startup_benchmarks(ctx):
    out = os.path.join(ctx.out, "startup")
    os.makedirs(out, exist_ok=True)
    src = os.path.join(ctx.here, "programs", "hello")
    rust = os.path.join(out, "hello-rust")
    rerr = rustc(ctx, "rustc hello", os.path.join(src, "hello.rs"), rust)
    velt = os.path.join(out, "hello-velt")
    verr = velt_build(ctx, os.path.join(src, "hello.vlt"), velt)
    sizes = {}
    for label, path in (("Rust", rust), ("Velt", velt), ("Node", ctx.node), ("Bun", ctx.bun)):
        if path and os.path.isfile(path):
            sizes[label] = os.path.getsize(os.path.realpath(path))
    bench = {
        "category": "startup", "name": "hello world", "check": "ref", "sizes": sizes,
        "runs_factor": 4,
        "impls": [impl("Rust", "rust", [rust], rerr), impl("Velt", "velt", [velt], verr),
                  *js_impls(ctx, os.path.join(src, "hello.js"))],
    }
    return [bench]


# ---- Compile time ----

def _rmtree(path):
    return lambda: shutil.rmtree(path, ignore_errors=True)


def _touch(path):
    return lambda: os.utime(path)


def compile_benchmarks(ctx):
    """Compiler wall time, cold: Velt with its build cache and build stamp off (every function
    compiled, std included), Rust with no compiler cache (RUSTC_WRAPPER unset)."""
    out = os.path.join(ctx.out, "compile")
    os.makedirs(out, exist_ok=True)
    cold = {"VELT_BUILD_CACHE": "off", "VELT_BUILD_STAMP": "off"}
    nocache = {"RUSTC_WRAPPER": "", "CARGO_BUILD_RUSTC_WRAPPER": ""}
    cases = [
        ("hello world", os.path.join(ctx.here, "programs", "hello", "hello.vlt"),
         os.path.join(ctx.here, "programs", "hello", "hello.rs")),
        ("nbody (CPU micro-benchmark)", os.path.join(ctx.bench, "nbody.vlt"),
         os.path.join(ctx.bench, "rust", "nbody.rs")),
    ]
    benches = []
    for name, vsrc, rsrc in cases:
        o = os.path.join(out, name.split()[0])
        benches.append({
            "category": "compile", "name": name, "check": "none",
            "impls": [impl("velt build (debug, Cranelift)", "velt", [ctx.velt, "build", vsrc, "-o", o + "-vd"], env=cold),
                      impl("velt build --release (LLVM -O3)", "velt", [ctx.velt, "build", "--release", vsrc, "-o", o + "-vr"], env=cold),
                      impl("rustc (debug)", "rust", ["rustc", "--edition", "2021", "-o", o + "-rd", rsrc], env=nocache),
                      impl("rustc -C opt-level=3, fat LTO, x86-64-v2 (as the runtime rows)", "rust",
                           [*RUSTC, "-C", f"target-cpu={RUST_CPU}", "-o", o + "-rr", rsrc], env=nocache)],
        })
    # The HTTP server: velt/http against axum. Cargo compiles axum, hyper and tokio from source
    # on a clean build (Velt's runtime, which holds its hyper and tokio, is prebuilt); the
    # rebuild row (main.rs touched, dependencies built) is the comparable edit-compile time.
    te = os.path.join(ctx.bench, "http", "techempower")
    axum_src = os.path.join(out, "axum-src")
    shutil.rmtree(axum_src, ignore_errors=True)
    shutil.copytree(os.path.join(te, "axum"), axum_src, ignore=shutil.ignore_patterns("target"))
    clean_t, touch_t = os.path.join(out, "axum-clean"), os.path.join(out, "axum-touch")
    main_rs = os.path.join(axum_src, "src", "main.rs")
    o = os.path.join(out, "server")
    cargo_cmd = ["cargo", "build", "--release", "-q", "--offline", "--locked"]
    relenv = {**nocache, **rust_profile()}
    benches.append({
        "category": "compile", "name": "HTTP server (techempower: velt/http vs axum)", "check": "none",
        "impls": [impl("velt build (debug, Cranelift)", "velt", [ctx.velt, "build", os.path.join(te, "server.vlt"), "-o", o + "-vd"], env=cold),
                  impl("velt build --release (LLVM -O3)", "velt", [ctx.velt, "build", "--release", os.path.join(te, "server.vlt"), "-o", o + "-vr"], env=cold),
                  impl("cargo build (debug), clean: axum + deps", "rust", ["cargo", "build", "-q", "--offline", "--locked", "--target-dir", clean_t],
                       env=nocache, cwd=axum_src, pre=_rmtree(clean_t)),
                  impl("cargo build --release (fat LTO, as the runtime rows), clean: axum + deps", "rust",
                       [*cargo_cmd, "--target-dir", clean_t], env=relenv, cwd=axum_src, pre=_rmtree(clean_t)),
                  impl("cargo build --release (fat LTO), main.rs touched (deps built)", "rust",
                       [*cargo_cmd, "--target-dir", touch_t], env=relenv, cwd=axum_src, pre=_touch(main_rs))],
    })
    # Fetch the crates once, outside the timing (the clean builds run --offline).
    ferr = sh(ctx, "cargo fetch (axum)", ["cargo", "fetch", "-q"], cwd=axum_src)
    if ferr:
        for i in benches[-1]["impls"][2:]:
            i["error"] = ferr
    return benches
