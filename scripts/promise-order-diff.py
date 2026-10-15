#!/usr/bin/env python3
"""Differential test of promise callback order: generates programs mixing `then`, `catch`,
`then(f, g)`, `finally`, `queueMicrotask`, `setTimeout`, the combinators and `await`, runs each
with `velt run` and with Node (`--experimental-strip-types`), and compares stdout and the exit
code.

Every program that differs is put in a class: `documented` when it uses a form whose order
docs/reference/async.md lists as different from Node's (`await` in `main` or in an async source
that awaits inside, `Promise.all`, `Promise.race`), `unexpected` otherwise. A non-zero count of
`unexpected` is a bug.

usage: promise-order-diff.py <velt executable> <work dir> [count] [seed] [jobs]
"""

import os
import random
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

PRELUDE = """class Failed extends Error {}

function log(s: string) {
  console.log(s);
}

async function value(n: number): Promise<number, Failed> {
  return n;
}

async function failing(m: string): Promise<number, Failed> {
  throw new Failed(m);
}

async function later(n: number): Promise<number, Failed> {
  await value(0);
  return n;
}
"""


class Gen:
    def __init__(self, rng):
        self.rng = rng
        self.features = set()
        self.n = 0

    def label(self, chain, what):
        self.n += 1
        return f"{chain} {what}{self.n}"

    def source(self, chain):
        """A source promise expression and whether it may reject."""
        r = self.rng.random()
        if r < 0.22:
            return f"Promise.resolve({self.rng.randint(1, 9)})", False
        if r < 0.40:
            return f"value({self.rng.randint(1, 9)})", True
        if r < 0.55:
            return f'failing("{chain}")', True
        if r < 0.63:
            self.features.add("new Promise")
            return f"new Promise<number, Failed>((resolve) => resolve({self.rng.randint(1, 9)}))", True
        if r < 0.70:
            self.features.add("allSettled")
            return (
                f"Promise.allSettled([value(1), failing(\"{chain}s\")]).then((rs) => rs.length)",
                False,
            )
        if r < 0.77:
            self.features.add("any")
            first = self.rng.choice(["value(5)", f'failing("{chain}a")'])
            return f"Promise.any([{first}, value(6)])", True
        if r < 0.82:
            self.features.add("all")
            return "Promise.all([value(1), value(2)]).then((xs) => xs.length)", True
        if r < 0.86:
            self.features.add("race")
            return "Promise.race([value(3), value(4)])", True
        if r < 0.92:
            self.features.add("source awaits inside")
            return f"later({self.rng.randint(1, 9)})", True
        self.features.add("withResolvers")
        return f"resolved{chain}", True

    def op(self, chain, may_reject):
        """One callback method call and whether the result may reject."""
        r = self.rng.random()
        l = self.label(chain, "")
        if r < 0.30:
            return f".then((v) => {{ log(`{l}then ${{v}}`); return v + 1; }})", may_reject
        if r < 0.40:
            self.features.add("callback returns a promise")
            return f".then((v) => {{ log(`{l}then-p ${{v}}`); return value(v + 1); }})", True
        if r < 0.48:
            return f'.then((v): number => {{ log(`{l}throw ${{v}}`); throw new Failed("{l}"); }})', True
        if r < 0.62 and may_reject:
            return f".catch((e) => {{ log(`{l}catch ${{e.message}}`); return 0; }})", False
        if r < 0.70 and may_reject:
            return (
                f".then((v) => {{ log(`{l}ok ${{v}}`); return v; }}, "
                f"(e) => {{ log(`{l}err ${{e.message}}`); return -1; }})",
                False,
            )
        if r < 0.85:
            self.features.add("finally")
            return f'.finally(() => log("{l}finally"))', may_reject
        if r < 0.93:
            self.features.add("finally async")
            return f'.finally(async () => {{ log("{l}finally-a"); }})', may_reject
        self.features.add("catch returns a promise")
        if may_reject:
            return f".catch((e) => {{ log(`{l}catch-p ${{e.message}}`); return value(7); }})", False
        return f".then((v) => {{ log(`{l}then ${{v}}`); return v; }})", may_reject

    def chain(self, name, allow_unhandled):
        src, may_reject = self.source(name)
        pre = ""
        if src.startswith("resolved"):
            pre = (
                f"  const w{name} = Promise.withResolvers<number, Failed>();\n"
                f"  w{name}.resolve({self.rng.randint(1, 9)});\n"
            )
            src = f"w{name}.promise"
        ops = []
        for _ in range(self.rng.randint(1, 4)):
            o, may_reject = self.op(name, may_reject)
            ops.append(o)
        if may_reject and not allow_unhandled:
            ops.append(f".catch((e) => {{ log(`{name} end ${{e.message}}`); return 0; }})")
            may_reject = False
        if may_reject:
            self.features.add("unhandled")
        body = "\n    ".join(ops)
        return f"{pre}  {src}\n    {body};\n"

    def program(self):
        parts = []
        chains = self.rng.randint(1, 4)
        unhandled = self.rng.random() < 0.08
        for i in range(chains):
            parts.append(self.chain(f"c{i}", unhandled and i == chains - 1))
            if self.rng.random() < 0.25:
                self.features.add("queueMicrotask")
                parts.append(
                    f'  queueMicrotask(() => {{ log("qm{i}"); '
                    f'queueMicrotask(() => log("qm{i} nested")); }});\n'
                )
        if self.rng.random() < 0.3:
            self.features.add("setTimeout")
            parts.insert(
                self.rng.randint(0, len(parts)),
                '  setTimeout(async () => log("timeout"), 0);\n',
            )
        if self.rng.random() < 0.15:
            self.features.add("await in main")
            parts.append('  log(`awaited ${await value(1)}`);\n')
        parts.append('  log("sync end");\n')
        return PRELUDE + "\nasync function main() {\n" + "".join(parts) + "}\n"


DOCUMENTED = {"await in main", "source awaits inside", "all", "race"}


def run(cmd, cwd):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, timeout=120)
    except subprocess.TimeoutExpired:
        return ("timeout", -1)
    return (p.stdout.decode("utf-8", "replace").replace("\r\n", "\n"), p.returncode)


def main():
    velt, work = sys.argv[1], sys.argv[2]
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 300
    seed = int(sys.argv[4]) if len(sys.argv) > 4 else 1
    os.makedirs(work, exist_ok=True)
    jobs = int(sys.argv[5]) if len(sys.argv) > 5 else 4
    rng = random.Random(seed)
    programs = []
    for i in range(count):
        g = Gen(rng)
        programs.append((i, g.program(), g.features))

    def check(item):
        i, src, _ = item
        vlt = os.path.join(work, f"p{i}.vlt")
        ts = os.path.join(work, f"p{i}.ts")
        with open(vlt, "w", newline="\n") as f:
            f.write(src)
        with open(ts, "w", newline="\n") as f:
            f.write(src + "\nmain();\n")
        v = run([velt, "run", vlt], work)
        n = run(["node", "--experimental-strip-types", "--no-warnings", ts], work)
        # Node exits with 1 on an unhandled rejection, as Velt does.
        return v == n

    with ThreadPoolExecutor(jobs) as pool:
        results = list(pool.map(check, programs))
    same = 0
    documented = []
    unexpected = []
    for (i, _, features), ok in zip(programs, results):
        if ok:
            same += 1
            continue
        doc = sorted(features & DOCUMENTED)
        (documented if doc else unexpected).append((i, sorted(features)))
    print(f"{count} programs (seed {seed}): {same} identical, {len(documented)} differ in a "
          f"documented form, {len(unexpected)} differ otherwise")
    for i, feats in unexpected:
        print(f"  unexpected p{i}: {', '.join(feats)}")
    for i, feats in documented:
        print(f"  documented p{i}: {', '.join(feats)}")
    return 1 if unexpected else 0


if __name__ == "__main__":
    sys.exit(main())
