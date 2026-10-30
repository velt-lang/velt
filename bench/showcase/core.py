"""Core features and TSX: the showcase categories that time one language feature at a time,
from the existing suites (bench/typical, iter, strings_utf16, key_order, any_ops, any_text,
typed_array, structs, btree, atomics, stdport) plus the TSX rendering programs of
bench/showcase/tsx. Same conventions as suites.py.
"""

import os
import subprocess

from suites import cargo, impl, js_impls, rustc, sh, velt_build

# bench/typical programs by the core feature they exercise (every one has rust/<name>.rs and
# runs in Node and Bun as TypeScript).
TYPICAL = {
    "collections": ["keys", "wordcount", "record", "objects", "sortcmp", "stats"],
    "strings": ["strings", "tokenize"],
    "language": ["callbacks", "chains", "consume", "dispatch", "errors", "fib", "graph", "grid",
                 "numloop", "trees", "vec2"],
}
TYPED_ARRAY = [("u8", 200), ("dot", 40), ("axpy", 100), ("i32", 100), ("xor", 1000), ("nested", 400)]


def as_ts(ctx, src, out_dir):
    """A copy of src that Node and Bun run as TypeScript, with the `main();` call Velt makes."""
    dst = os.path.join(out_dir, os.path.splitext(os.path.basename(src))[0] + ".ts")
    with open(src, encoding="utf-8") as f, open(dst, "w", encoding="utf-8") as g:
        g.write(f.read() + "\nmain();\n")
    return dst


def core_benchmarks(ctx):
    out = os.path.join(ctx.out, "core")
    os.makedirs(out, exist_ok=True)
    b = ctx.bench
    benches = []

    # bench/typical: Velt, Rust (suites.rustc), Node and Bun on the same TypeScript.
    for area, names in TYPICAL.items():
        for name in names:
            if not ctx.wanted_name(name):
                continue
            src = os.path.join(b, "typical", f"{name}.vlt")
            rust = os.path.join(out, f"typical-{name}-rust")
            rerr = rustc(ctx, f"rustc typical/{name}", os.path.join(b, "typical", "rust", f"{name}.rs"), rust)
            velt = os.path.join(out, f"typical-{name}")
            verr = velt_build(ctx, src, velt)
            ts = as_ts(ctx, src, out)
            benches.append({"category": "core", "area": area, "name": f"{area}: {name} (typical)", "check": "ref",
                            "impls": [impl("Rust", "rust", [rust], rerr), impl("Velt", "velt", [velt], verr),
                                      *js_impls(ctx, ts)]})

    # Iteration, non-ASCII strings, object key order, `any`, typed arrays: no Rust program in
    # the suites (Rust has no such dynamic or UTF-16 semantics to compare): Node is the reference.
    js_suites = [
        ("language", "iter", lambda n: os.path.join(b, "iter", "node", f"{n}.js"),
         sorted(f[:-4] for f in os.listdir(os.path.join(b, "iter")) if f.endswith(".vlt"))),
        ("strings", "strings_utf16", lambda n: os.path.join(b, "strings_utf16", f"{n}.js"),
         sorted(f[:-4] for f in os.listdir(os.path.join(b, "strings_utf16")) if f.endswith(".vlt"))),
        ("collections", "key_order", lambda n: os.path.join(b, "key_order", f"{n}.js"),
         sorted(f[:-4] for f in os.listdir(os.path.join(b, "key_order")) if f.endswith(".vlt"))),
    ]
    for area, suite, js, names in js_suites:
        for name in names:
            if not ctx.wanted_name(name) or not os.path.isfile(js(name)):
                continue
            velt = os.path.join(out, f"{suite}-{name}")
            verr = velt_build(ctx, os.path.join(b, suite, f"{name}.vlt"), velt)
            benches.append({"category": "core", "area": area, "name": f"{area}: {name} ({suite})", "check": "ref",
                            "impls": [*js_impls(ctx, js(name)), impl("Velt", "velt", [velt], verr),
                                      impl("Rust", "rust", [], "n/a: no Rust program in the suite")]})
    for suite in ("any_ops", "any_text"):
        for f in sorted(os.listdir(os.path.join(b, suite))):
            if not f.endswith(".ts") or not ctx.wanted_name(f[:-3]):
                continue
            src = os.path.join(b, suite, f)
            velt = os.path.join(out, f"{suite}-{f[:-3]}")
            verr = velt_build(ctx, src, velt)
            benches.append({"category": "core", "area": "language", "name": f"language: any {f[:-3]} ({suite})",
                            "check": "ref",
                            "impls": [*js_impls(ctx, src), impl("Velt", "velt", [velt], verr),
                                      impl("Rust", "rust", [], "n/a: `any` is dynamic typing; Rust has none")]})
    loops = os.path.join(b, "typed_array", "loops.ts")
    velt = os.path.join(out, "typed-array-loops")
    verr = velt_build(ctx, loops, velt)
    ts = as_ts(ctx, loops, out)
    for kernel, reps in TYPED_ARRAY:
        if not ctx.wanted_name(kernel):
            continue
        reps = str(max(1, reps // 50) if ctx.smoke else reps)
        benches.append({"category": "core", "area": "collections", "name": f"collections: typed array {kernel} x{reps}",
                        "check": "ref",
                        "impls": [*js_impls(ctx, ts, [kernel, reps]), impl("Velt", "velt", [velt, kernel, reps], verr),
                                  impl("Rust", "rust", [], "n/a: no Rust program in the suite")]})

    # Value types (bench/structs): Velt struct and class against a Rust Copy struct; Node and Bun
    # run the class version (a struct is Velt syntax).
    for name in ("mapkey", "particles", "points", "vecmath"):
        if not ctx.wanted_name(name):
            continue
        rust = os.path.join(out, f"structs-{name}-rust")
        rerr = rustc(ctx, f"rustc structs/{name}", os.path.join(b, "structs", "rust", f"{name}.rs"), rust)
        impls = [impl("Rust (Copy struct)", "rust", [rust], rerr)]
        for kind in ("struct", "class"):
            exe = os.path.join(out, f"structs-{name}-{kind}")
            err = velt_build(ctx, os.path.join(b, "structs", f"{name}_{kind}.vlt"), exe)
            impls.append(impl(f"Velt {kind}", "velt", [exe], err))
        ts = as_ts(ctx, os.path.join(b, "structs", f"{name}_class.vlt"), out)
        impls += js_impls(ctx, ts, label_suffix=" class")
        benches.append({"category": "core", "area": "language", "name": f"language: {name} (structs)",
                        "check": "ref", "impls": impls})

    # Ordered maps and atomics: Velt against Rust (rust/.cargo/config.toml builds for this CPU,
    # so Velt does too). The programs time each workload themselves: `<name> <ms> <check>`.
    for suite, src, label, bins in (
            ("btree", "btree.vlt", "Velt BTreeMap", [("Rust std BTreeMap", "std"), ("Rust bbtree", "bbtree")]),
            ("atomics", "atomics.vlt", "Velt Atomic<T>", [("Rust std::sync::atomic", "atomics_bench")])):
        if not ctx.wanted_name(suite):
            continue
        area = "collections" if suite == "btree" else "concurrency"
        target = os.path.join(out, f"{suite}-rust")
        rerr = cargo(ctx, f"{suite} Rust baseline", os.path.join(b, suite, "rust"), target, native=True)
        exe = os.path.join(out, suite)
        verr = velt_build(ctx, os.path.join(b, suite, src), exe, ["--cpu", "native"])
        js_na = ("n/a: JavaScript has no built-in sorted map" if suite == "btree"
                 else "n/a: no JS program in the suite (Atomics on a SharedArrayBuffer needs workers)")
        benches.append({"category": "core", "area": area, "name": f"{area}: {suite}", "check": "named",
                        "parse": "named", "unit": "ms",
                        "impls": [*[impl(l, "rust", [os.path.join(target, "release", bn)], rerr) for l, bn in bins],
                                  impl(label, "velt", [exe], verr),
                                  impl("Node", "node", [], js_na), impl("Bun", "bun", [], js_na)]})

    # Std components against the Rust crate for the same job (bench/stdport): number formatting
    # and parsing, UTF-8, base64, hex, SHA-256, URLs, big integers, decompression. One thread, as
    # its own harness runs them.
    ports = sorted(f[:-4] for f in os.listdir(os.path.join(b, "stdport")) if f.endswith(".vlt"))
    ports = [n for n in ports if ctx.wanted_name(n) or ctx.wanted_name("stdport")]
    if ports:
        target = os.path.join(out, "stdport-rust")
        rerr = cargo(ctx, "stdport Rust crates", os.path.join(b, "stdport", "rust"), target)
        inputs = os.path.join(out, "stdport-inputs")
        if not rerr:
            rerr = sh(ctx, "stdport inputs", [os.path.join(target, "release", "gen_inputs"), inputs])
        env = {"VELT_THREADS": "1", "STDPORT_INPUTS": inputs}
        for name in ports:
            exe = os.path.join(out, f"stdport-{name}")
            verr = velt_build(ctx, os.path.join(b, "stdport", f"{name}.vlt"), exe)
            benches.append({"category": "core", "area": "strings", "name": f"std: {name} (stdport)", "check": "ref",
                            "impls": [impl("Rust crate", "rust", [os.path.join(target, "release", name)], rerr, env=env),
                                      impl("Velt std", "velt", [exe], verr, env=env)]})
    return benches


# ---- TSX: server-side rendering (bench/showcase/tsx) ----

def npm_install(ctx, cwd, what):
    if os.path.isdir(os.path.join(cwd, "node_modules")):
        return None
    lock = os.path.isfile(os.path.join(cwd, "package-lock.json"))
    return sh(ctx, f"npm {'ci' if lock else 'install'} ({what})",
              [ctx.npm or "npm", "ci" if lock else "install", "--no-audit", "--no-fund"], cwd=cwd)


def tsx_benchmarks(ctx):
    here = os.path.join(ctx.here, "tsx")
    out = os.path.join(ctx.out, "tsx")
    os.makedirs(out, exist_ok=True)
    velt = os.path.join(out, "render")
    verr = velt_build(ctx, os.path.join(here, "render.vlt"), velt)
    nerr = npm_install(ctx, here, "react, react-dom, preact, preact-render-to-string")
    scale = ["10"] if ctx.smoke else ["1"]
    prod = {"NODE_ENV": "production"}
    impls = [impl("Velt TSX (velt:jsx renderToStringSync)", "velt", [velt, "both", *scale], verr)]
    for lib, name in (("react", "React renderToString"), ("preact", "Preact preact-render-to-string")):
        for lang, exe in (("node", ctx.node), ("bun", ctx.bun)):
            err = nerr or (None if exe else f"{lang} not found")
            impls.append(impl(f"{'Node' if lang == 'node' else 'Bun'} {name}", lang,
                              [exe or lang, os.path.join(here, "render.mjs"), lib, "both", *scale], err, env=prod))
    impls.append(impl("Rust", "rust", [], "n/a: no JSX; a template engine (askama, maud) would be templating, not JSX"))
    # The pages must be the same bytes, not only the same length: one untimed comparison.
    checks = []
    for page in ("small", "large"):
        checks.append({"category": "tsx", "name": f"same HTML: {page} page", "check": "ref",
                       "impls": [impl("Velt", "velt", [velt, "html", page], verr),
                                 *[impl(f"Node {lib}", "node", [ctx.node or "node", os.path.join(here, "render.mjs"), lib, "html", page],
                                        nerr, env=prod) for lib in ("react", "preact")]],
                       "runs_factor": 0})
    return [{"category": "tsx", "name": "render to string (10-row page x20000, 1000-row page x500)",
             "check": "results", "parse": "results", "unit": "ops/s", "impls": impls}, *checks]


def tsx_servers(ctx, server_threads):
    """([(label, lang, cmds(port), env, error)] for the TSX page servers, like
    servers.http_servers, and a function giving the expected bodies {route: bytes}: the pages
    render.vlt prints, which the tsx category compares with React's and Preact's)."""
    here = os.path.join(ctx.here, "tsx")
    out = os.path.join(ctx.out, "tsx")
    os.makedirs(out, exist_ok=True)
    velt = os.path.join(out, "server")
    verr = velt_build(ctx, os.path.join(here, "server.vlt"), velt)
    nerr = npm_install(ctx, here, "react, react-dom, preact, preact-render-to-string")
    js = os.path.join(here, "server.mjs")
    servers = [("Velt TSX", "velt", lambda p: [[velt, str(p)]], {"VELT_THREADS": str(server_threads)}, verr)]
    for lib, name in (("react", "React"), ("preact", "Preact")):
        servers.append((f"Node node:http + {name} (1 process)", "node",
                        lambda p, lib=lib: [[ctx.node or "node", js, lib, str(p)]], {"NODE_ENV": "production"},
                        nerr or (None if ctx.node else "node not found")))
        servers.append((f"Bun Bun.serve + {name} (1 process)", "bun",
                        lambda p, lib=lib: [[ctx.bun or "bun", js, lib, str(p)]], {"NODE_ENV": "production"},
                        nerr or (None if ctx.bun else "bun not found")))
    render = os.path.join(out, "render")
    if not os.path.isfile(render):
        velt_build(ctx, os.path.join(here, "render.vlt"), render)

    def expected():
        pages = {}
        for page in ("small", "large"):
            p = subprocess.run([render, "html", page], capture_output=True)
            if p.returncode == 0:
                # console.log's newline is not part of the page; the server prepends the doctype.
                pages[page] = b"<!DOCTYPE html>" + p.stdout.rstrip(bytes([10]))
        return pages
    return servers, expected

