#!/usr/bin/env python3
"""Whether a probe executes the same number of instructions whatever its secret.

A probe (bench/crypto/ct/<name>.probe names it: a program there, or a white-box test file in
std for what programs cannot import) reads a secret from the file it is given and runs the code
under test on it. This builds
the probe in release mode and runs it under valgrind's cachegrind once per secret: edge values
and random ones. Constant-time code executes one count; a second count means some branch or loop
depends on the secret, in the machine code if not in the source (LLVM turns a mask it can see
through into a branch).

What this cannot see: a memory address that depends on the secret with no change in the count,
and a difference that is rarer than the secrets tried.
The memcheck command in std/crypto/field_p256.velt is the check for that.

  scripts/ct-counts.py p384            # one probe
  scripts/ct-counts.py                 # all of them
  scripts/ct-counts.py --runs 70 p384

Needs valgrind and a built `velt` (target/debug/velt, or $VELT). Exit status 1 when a probe has
more than one count.
"""
import argparse, hashlib, hmac, json, os, random, re, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
N384 = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFC7634D81F4372DDF581A0DB248B0A77AECEC196ACCC52973


def scalars384(rng, runs):
    """Valid P-384 private keys: small, large, one digit set, then random."""
    edge = [1, 2, 15, 16, 17, N384 - 1, N384 - 2, N384 - 16, 1 << 380, 0xF << 380, 1 << 191,
            (N384 - 1) // 2, int("0f" * 48, 16), int("11" * 48, 16), 0xB << 56, 1 << 4]
    values = edge + [rng.randrange(1, N384) for _ in range(max(0, runs - len(edge)))]
    return [v.to_bytes(48, "big") for v in values[:max(runs, len(edge))]]


def seeds32(rng, runs):
    """Ed25519 seeds: any 32 bytes."""
    edge = [bytes(32), b"\xff" * 32, b"\x01" + bytes(31), bytes(31) + b"\x80",
            bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")]
    return edge + [rng.randbytes(32) for _ in range(max(0, runs - len(edge)))]


P384 = 2**384 - 2**128 - 2**96 + 2**32 - 1
G384 = (0xAA87CA22BE8B05378EB1C71EF320AD746E1D3B628BA79B9859F741E082542A385502F25DBF55296C3A545E3872760AB7,
        0x3617DE4A96262C6F5D9E98BF9292DC29F8F41DBD289A147CE9DA3113B5F0B8C00A60B1CE1D7E819D7A431D7C90EA0E5F)


def _add384(a, b):
    if a is None or b is None:
        return a or b
    if a[0] == b[0]:
        if (a[1] + b[1]) % P384 == 0:
            return None
        k = (3 * a[0] * a[0] - 3) * pow(2 * a[1], -1, P384) % P384
    else:
        k = (b[1] - a[1]) * pow(b[0] - a[0], -1, P384) % P384
    x = (k * k - a[0] - b[0]) % P384
    return x, (k * (a[0] - x) - a[1]) % P384


def _signature384(key, digest):
    """(r, s) of ECDSA on P-384 with RFC 6979's nonce over HMAC-SHA384, as std's `p384Sign`."""
    h = (int.from_bytes(digest, "big") % N384).to_bytes(48, "big")
    mac = lambda k, m: hmac.new(k, m, hashlib.sha384).digest()
    v, k = b"\x01" * 48, bytes(48)
    k = mac(k, v + b"\x00" + key + h)
    v = mac(k, v)
    k = mac(k, v + b"\x01" + key + h)
    v = mac(k, v)
    while True:
        v = mac(k, v)
        nonce = int.from_bytes(v, "big")
        if 0 < nonce < N384:
            point, base, n = None, G384, nonce
            while n:
                if n & 1:
                    point = _add384(point, base)
                base, n = _add384(base, base), n >> 1
            r = point[0] % N384
            s = pow(nonce, -1, N384) * (int.from_bytes(h, "big") + r * int.from_bytes(key, "big")) % N384
            if r and s:
                return r, s
        k = mac(k, v + b"\x00")
        v = mac(k, v)


def signers384(rng, runs):
    """P-384 private keys whose signature of the signing probe's digest has one DER shape.

    A signature's encoding depends on r and s, which are public: an integer with its top bit set
    takes one more byte. Keys are kept when both integers are 48 bytes with the top bit clear, so
    that what is left to differ is the key.
    """
    digest = bytes((3 * i + 1) & 0xFF for i in range(48))
    kept = []
    for key in scalars384(rng, 4 * runs + 64):
        r, s = _signature384(key, digest)
        if all(1 << 376 <= x < 1 << 383 for x in (r, s)):
            kept.append(key)
        if len(kept) == runs:
            break
    return kept


# How often a secret whose count differs is run again before the difference counts.
RETRIES = 5

GENERATORS = {"scalars384": scalars384, "seeds32": seeds32, "signers384": signers384}


def probes():
    """The probes: bench/crypto/ct/<name>.probe, one line `kind source secrets`.

    `kind` is `program` (a Velt program, built with `velt build`) or `test` (a test file, for
    code a program cannot import: the harness's executable is run), `source` is relative to the
    repository, and `secrets` names a generator above. The probe gets the secret's file as its
    first argument and in $VELT_CT_SECRET.
    """
    found = {}
    folder = os.path.join(ROOT, "bench", "crypto", "ct")
    for entry in sorted(os.listdir(folder)):
        if entry.endswith(".probe"):
            with open(os.path.join(folder, entry)) as f:
                kind, source, secrets = f.read().split()
            found[entry[:-len(".probe")]] = (source, kind, GENERATORS[secrets])
    return found


def velt():
    return os.environ.get("VELT") or os.path.join(ROOT, "target", "debug", "velt")


def build(source, kind, out):
    """The probe's executable and the environment it runs in."""
    path = os.path.join(ROOT, source)
    if kind == "program":
        exe = os.path.join(out, "probe")
        subprocess.run([velt(), "build", "--release", path, "-o", exe], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return exe, {}
    # A test file: the harness's executable, which runs the file's tests.
    made = subprocess.run([velt(), "test", path, "--release", "--no-run", "--json"], check=True,
                          capture_output=True, text=True, cwd=ROOT)
    harness = json.loads(made.stdout)["harnesses"][0]
    return harness["executable"], harness.get("env", {})


def count(exe, env, secret_file):
    run = subprocess.run(
        ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null",
         exe, secret_file],
        capture_output=True, text=True, env={**os.environ, **env, "VELT_CT_SECRET": secret_file})
    found = re.search(r"I\s+refs:\s+([\d,]+)", run.stderr)
    if run.returncode != 0 or not found:
        sys.exit(f"the probe failed under valgrind:\n{run.stdout}{run.stderr}")
    return int(found.group(1).replace(",", ""))


def main():
    args = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    known = probes()
    args.add_argument("probes", nargs="*", help=f"which probes (default: all of {sorted(known)})")
    args.add_argument("--runs", type=int, default=40, help="secrets per probe (default 40)")
    args.add_argument("--seed", type=int, default=2392, help="seed of the random secrets")
    opts = args.parse_args()
    if not shutil.which("valgrind"):
        sys.exit("valgrind is needed")
    failed = False
    for name in opts.probes or sorted(known):
        if name not in known:
            sys.exit(f"no probe named {name}; there are {sorted(known)}")
        source, kind, secrets = known[name]
        with tempfile.TemporaryDirectory() as out:
            exe, env = build(source, kind, out)
            # One path for every run: its length is part of what the process does.
            secret_file = os.path.join(out, "secret.bin")
            counts = {}
            target = None
            for secret in secrets(random.Random(opts.seed), opts.runs):
                with open(secret_file, "wb") as f:
                    f.write(secret)
                # A test harness starts threads, and now and then one of them does more before
                # the process ends: the same secret then counts some thousands more. So a count
                # that differs is taken again, and the lowest of a few runs is the secret's.
                # Code that depends on the secret differs every time.
                tries = RETRIES if target is None else 1
                c = min(count(exe, env, secret_file) for _ in range(tries))
                for _ in range(RETRIES):
                    if target is None or c == target:
                        break
                    c = min(c, count(exe, env, secret_file))
                target = c if target is None else target
                counts.setdefault(c, []).append(secret)
        runs = sum(len(v) for v in counts.values())
        if len(counts) == 1:
            print(f"{name}: {runs} secrets, one count: {next(iter(counts)):,} instructions")
        else:
            failed = True
            low, high = min(counts), max(counts)
            print(f"{name}: NOT CONSTANT: {len(counts)} different counts over {runs} secrets, "
                  f"{low:,} to {high:,}")
            for c in sorted(counts)[:4]:
                print(f"  {c:,}: {counts[c][0].hex()}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
