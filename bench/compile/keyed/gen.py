#!/usr/bin/env python3
"""Programs for the compile cost of the check of stores by key (#854): N classes and N function
values, as `crates/velt_sema/tests/scaling.rs` generates them, with the HTTP server's `serve` as the
crossing. Compile with `velt check` from a directory whose package depends on `http-server`
(tests/golden/lang/errors/http_server has one).

    gen.py <shape> <n> <c> > program.vlt

Shapes (`crates/velt_sema/src/ownership/local_async/keyed`), each compiling without an error:
    A   every class stores by one open key, and calls a hook with what it holds
    B   each class stores by a constant key of its own, and calls a hook with what it holds
    B2  the same, and it passes its parameter on: the functions form one cycle through the hooks
    T   N function types, each with its own values
    TK  as A, and N function types with a constant key per type, each class converted to `any` and
        given a value that is not new
    TW  N function types `(a: any, d: D{i}) => void`, with N values `(d: D{i}, v: any) => void`
    L   as A, and N classes in a cycle through hooks, each calling the function it is given with a
        box of its own type
`c` is the length of a chain of helpers ending in a call of a hook.
"""
import sys


def store_shape(shape, n, c):
    line = {
        "A": 'p[key + ""] = v; hooks[{m}](this.peer, v);',
        "B": 'p["k{i}"] = v; hooks[{m}](this.peer, v);',
        "B2": 'p["k{i}"] = v; hooks[{m}](p, v);',
    }[shape]
    out = ["type Hook = (o: any, v: any) => void;", "const hooks: Hook[] = [];"]
    for i in range(n):
        out.append(
            f"class C{i} {{ peer: any = null; "
            f"put(p: any, key: string, v: any): void {{ {line.format(i=i, m=i % 7)} }} }}"
        )
    out.append("function setup(o: any): void {")
    for i in range(n):
        out.append(f"  const c{i}: any = new C{i}();")
        out.append(
            f"  hooks.push((a: any, b: any): void => {{ new C{i}().put(a, \"out\", b); }});"
        )
    out.append("  chain0(o, 1);")
    out.append("}")
    for i in range(c):
        out.append(f"function chain{i}(o: any, v: any): void {{ chain{i + 1}(o, v); }}")
    out.append(f"function chain{c}(o: any, v: any): void {{ hooks[0](o, v); }}")
    return out


def latent_shape(n, c):
    out = store_shape("A", n, c)
    out.insert(2, "const lhooks: ((a: any, f: (o: any, x: any) => void) => void)[] = [];")
    setup = out.index("function setup(o: any): void {")
    classes, pushes = [], []
    for i in range(n):
        classes.append(
            f"class Box{i} {{ items: string[] = []; }}\n"
            f"class L{i} {{ box: Box{i} = new Box{i}(); "
            f"put(p: any, f: (o: any, x: any) => void): void {{ "
            f"f(p, this.box); lhooks[{i % 7}](p, f); }} }}"
        )
        pushes.append(
            f"  lhooks.push((a: any, f: (o: any, x: any) => void): void => "
            f"{{ new L{i}().put(a, f); }});"
        )
    pushes.append('  lhooks[0](o, (p: any, x: any): void => { p["out"] = x; });')
    return out[:setup] + classes + out[setup:setup + 1] + pushes + out[setup + 1:]


def keyed_types_shape(n, c):
    out = store_shape("A", n, c)
    setup = out.index("function setup(o: any): void {")
    classes, runs = ["class Own { list: string[] = []; }"], ["  const own = new Own();"]
    for i in range(n):
        classes.append(
            f"class D{i} {{ out: any = null; k{i}: string[] = []; "
            f"cb: (d: D{i}, v: any) => void = (d: D{i}, v: any): void => "
            f'{{ const o: any = d; o["k{i}"] = v; }}; '
            f"run(v: any): void {{ this.cb(this, v); }} }}\n"
            f"const dconv{i}: any = new D{i}();"
        )
        runs.append(f"  new D{i}().run(own.list);")
    return out[:setup] + classes + out[setup:setup + 1] + runs + out[setup + 1:]


def wide_types_shape(n):
    out = []
    for i in range(n):
        out.append(f"class D{i} {{ out: any = null; }}")
        out.append(f"type W{i} = (a: any, d: D{i}) => void;")
        out.append(f"const w{i}: W{i}[] = [];")
        out.append(
            f"const v{i}: (d: D{i}, v: any) => void = (d: D{i}, v: any): void => "
            f'{{ const o: any = d; o["out"] = v; }};'
        )
    out.append("function setup(o: any): void {")
    for i in range(n):
        out.append(f"  v{i}(new D{i}(), 1);")
        out.append(f"  if (w{i}.length > 0) {{ w{i}[0](o, new D{i}()); }}")
    out.append("}")
    return out


def types_shape(n):
    out = []
    for i in range(n):
        out.append(
            f"class D{i} {{ out: any = null; "
            f"cb: (d: D{i}, v: any) => void = (d: D{i}, v: any): void => "
            f'{{ const o: any = d; o["out"] = v; }}; '
            f"run(v: any): void {{ this.cb(this, v); }} }}"
        )
    out.append("function setup(o: any): void {")
    for i in range(n):
        out.append(f'  new D{i}().run("x");')
    out.append("}")
    return out


def program(shape, n, c):
    body = {
        "T": lambda: types_shape(n),
        "TK": lambda: keyed_types_shape(n, c),
        "TW": lambda: wide_types_shape(n),
        "L": lambda: latent_shape(n, c),
    }.get(shape, lambda: store_shape(shape, n, c))()
    return "\n".join(
        ['import { serve } from "http-server";', "class App { plugins: string[] = []; }"]
        + body
        + [
            "async function main() {",
            "  const app = new App();",
            "  const s = await serve({ port: 0 }, async (req: Request): Promise<Response> => {",
            "    const own: any = { n: 1 };",
            "    setup(own);",
            "    return new Response(`${app.plugins.length}`);",
            "  });",
            "  await s.shutdown();",
            "}",
            "",
        ]
    )


if __name__ == "__main__":
    sys.stdout.write(program(sys.argv[1], int(sys.argv[2]), int(sys.argv[3])))
