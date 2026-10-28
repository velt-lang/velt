#!/usr/bin/env python3
"""Random JSON documents for bench/json/fuzz/values (one per line, written to stdout): objects
with repeated, array-index-like and `__proto__` keys, small and wide (over 16 keys), escapes,
lone surrogates, numbers in every form, and nesting. Deterministic (seeded)."""
import random, sys

r = random.Random(int(sys.argv[1]) if len(sys.argv) > 1 else 1660)

def string():
    parts = []
    for _ in range(r.randint(0, 8)):
        k = r.random()
        if k < 0.5:
            parts.append(r.choice("abcxyz _-é日😀"))
        elif k < 0.6:
            parts.append(r.choice(['\\n', '\\t', '\\"', '\\\\', '\\/', '\\u0001', '\\u00e9']))
        elif k < 0.7:
            parts.append('\\u%04x' % r.randint(0xd800, 0xdfff))
        elif k < 0.8:
            parts.append(r.choice(["0", "1", "17", "4294967294", "4294967295", "01", "-1", "1.5"]))
        else:
            parts.append(r.choice(["id", "name", "a", "b"]))
    return '"' + "".join(parts) + '"'

def number():
    k = r.random()
    if k < 0.3:
        return str(r.randint(-1000, 1000))
    if k < 0.6:
        return "%d.%d" % (r.randint(-99, 99), r.randint(0, 999))
    if k < 0.8:
        return "%de%d" % (r.randint(1, 9), r.randint(-30, 30))
    return r.choice(["0", "-0", "1e21", "1E+2", "0.1", "123456789012345678", "-0.0", "1.7976931348623157e308", "5e-324"])

def value(depth):
    k = r.random()
    if depth > 6 or k < 0.35:
        return r.choice([number, string, lambda: r.choice(["true", "false", "null"])])()
    if k < 0.65:
        return "[" + ",".join(value(depth + 1) for _ in range(r.randint(0, 5))) + "]"
    # Now and then an object over 16 keys (the tape's hash index), deep enough that its values
    # are scalars.
    n = r.randint(17, 40) if depth >= 6 and r.random() < 0.2 else r.randint(0, 6)
    keys = [r.choice([string(), '"a"', '"b"', '"0"', '"2"', '"10"', '"1"', '"id"', '"__proto__"']) for _ in range(n)]
    ws = r.choice(["", " ", "\t "])
    return "{" + ",".join(ws + key + ":" + ws + value(depth + 1) for key in keys) + "}"

for _ in range(int(sys.argv[2]) if len(sys.argv) > 2 else 20000):
    print(value(0))
