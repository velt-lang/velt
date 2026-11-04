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
    body = types_shape(n) if shape == "T" else store_shape(shape, n, c)
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
