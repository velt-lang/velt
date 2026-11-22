#!/usr/bin/env python3
"""Whether a probe executes the same number of instructions whatever its secret.

A probe (bench/crypto/ct/<name>.probe names it: a program there, or a white-box test file in
std for what programs cannot import) reads a secret from the file it is given and runs the code
under test on it. This builds
the probe in release mode and runs it under valgrind's cachegrind once per secret: edge values
and random ones. Constant-time code executes one count; a second count means some branch or loop
depends on the secret, in the machine code if not in the source (LLVM turns a mask it can see
through into a branch). With scripts/ct-shim.c (a C compiler and valgrind's headers), what is
counted starts at the probe's read of its secret and ends at its read of $VELT_CT_STOP's file
(or at the end): the code under test, without the process's start and end, where a test
harness's threads do more or less from run to run whatever the secret.

What the counts cannot see: a branch whose two sides cost the same, a memory address that
depends on the secret, and a difference that is rarer than the secrets tried. `--memcheck` sees
the first two: it runs the probe under valgrind's memcheck with the secret marked undefined
(scripts/ct-shim.c) and counts the reports of a conditional jump or an address that depends on
it. A probe must not branch on what it computed; what it does by design (a result check) is the
number its .probe file allows.
The memcheck command in std/crypto/field_p256.velt is the check for that.

  scripts/ct-counts.py p384            # one probe
  scripts/ct-counts.py                 # all of them
  scripts/ct-counts.py --runs 70 p384
  scripts/ct-counts.py --memcheck      # branches and addresses that depend on the secret

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
    return edge + [rng.getrandbits(256).to_bytes(32, "little") for _ in range(max(0, runs - len(edge)))]


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


def _prime(rng, bits):
    """A random prime of exactly `bits` bits with its two top bits set (Miller-Rabin)."""
    small = [3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97]
    while True:
        p = rng.getrandbits(bits) | (3 << (bits - 2)) | 1
        if any(p % q == 0 for q in small) or p % 65537 == 1:
            continue
        d, twos = p - 1, 0
        while d % 2 == 0:
            d, twos = d // 2, twos + 1
        for _ in range(24):
            x = pow(rng.randrange(2, p - 1), d, p)
            if x in (1, p - 1):
                continue
            for _ in range(twos - 1):
                x = x * x % p
                if x == p - 1:
                    break
            else:
                break
        else:
            return p


def keys2048(rng, runs):
    """RSA-2048 private keys for the RSA probe, as (secret, public): the secret is p, q, dP, dQ
    and qInv, 128 big-endian bytes each; the public part is n, 256 bytes (e is 65537). A few are
    enough: each takes two primes to make."""
    keys = []
    for _ in range(min(runs, 8)):
        p, q = _prime(rng, 1024), _prime(rng, 1024)
        d = pow(65537, -1, (p - 1) * (q - 1))
        secret = b"".join(v.to_bytes(128, "big") for v in (p, q, d % (p - 1), d % (q - 1), pow(q, -1, p)))
        keys.append((secret, (p * q).to_bytes(256, "big")))
    return keys


# How often a secret whose count differs is run again before the difference counts.
RETRIES = 5

GENERATORS = {"scalars384": scalars384, "seeds32": seeds32, "signers384": signers384,
              "keys2048": keys2048}


def probes():
    """The probes: bench/crypto/ct/<name>.probe, one line `kind source secrets [allowed]`.

    `kind` is `program` (a Velt program, built with `velt build`) or `test` (a test file, for
    code a program cannot import: the harness's executable is run), `source` is relative to the
    repository, and `secrets` names a generator above. The probe gets the secret's file as its
    first argument and in $VELT_CT_SECRET, and what goes with it and is public (an RSA modulus)
    in the file of that name with `.pub` added. `allowed` is how many memcheck reports the probe
    makes by design (--memcheck), 0 when left out.
    """
    found = {}
    folder = os.path.join(ROOT, "bench", "crypto", "ct")
    for entry in sorted(os.listdir(folder)):
        if entry.endswith(".probe"):
            with open(os.path.join(folder, entry)) as f:
                fields = f.read().split()
            kind, source, secrets = fields[:3]
            allowed = int(fields[3]) if len(fields) > 3 else 0
            found[entry[:-len(".probe")]] = (source, kind, GENERATORS[secrets], allowed)
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


# One runtime thread and no rescue thread, as bench/nightly.sh sets for its cachegrind rows: a
# test harness's worker threads otherwise add thousands of instructions that differ from run to
# run on a busy machine, and every count comes out different whatever the secret.
ONE_THREAD = {"VELT_THREADS": "1", "VELT_RESCUE": "0"}


def count(exe, env, secret_file, shim=None):
    """The instructions the probe executes for the secret in `secret_file`. With the shim, only
    from its read of the secret to its read of the stop file (or to the end): the code under
    test, without the process's start and end."""
    region = ["--instr-at-start=no"] if shim else []
    extra = {"LD_PRELOAD": shim, "VELT_CT_STOP": stop_file(secret_file)} if shim else {}
    run = subprocess.run(
        ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null",
         *region, exe, secret_file],
        capture_output=True, text=True,
        env={**os.environ, **env, **ONE_THREAD, "VELT_CT_SECRET": secret_file, **extra})
    found = re.search(r"I\s+refs:\s+([\d,]+)", run.stderr)
    if run.returncode != 0 or not found:
        sys.exit(f"the probe failed under valgrind:\n{run.stdout}{run.stderr}")
    return int(found.group(1).replace(",", ""))


def stop_file(secret_file):
    """The file whose first read ends what is counted (an empty file next to the secret's)."""
    return secret_file + ".stop"


def build_shim(out):
    """scripts/ct-shim.c built into `out`, or None (with a note) without a C compiler or
    valgrind's headers: counts then take the whole process."""
    shim = os.path.join(out, "ct-shim.so")
    made = subprocess.run(["cc", "-shared", "-fPIC", "-O1", "-o", shim,
                           os.path.join(ROOT, "scripts", "ct-shim.c"), "-ldl"],
                          capture_output=True, text=True)
    if made.returncode != 0:
        print("note: scripts/ct-shim.c does not build here (a C compiler and valgrind's headers);"
              " counting whole processes", file=sys.stderr)
        return None
    return shim


def counts_regions():
    """Whether cachegrind can start and stop counting on request (valgrind 3.22 and later)."""
    found = re.search(r"(\d+)\.(\d+)", subprocess.run(["valgrind", "--version"],
                                                    capture_output=True, text=True).stdout)
    return found is not None and (int(found.group(1)), int(found.group(2))) >= (3, 22)


def put(secret_file, secret):
    """Writes a secret, or a (secret, public) pair, where the probe reads it."""
    public = b""
    if isinstance(secret, tuple):
        secret, public = secret
    with open(secret_file, "wb") as f:
        f.write(secret)
    with open(secret_file + ".pub", "wb") as f:
        f.write(public)
    return secret


def reports(exe, env, secret_file, shim):
    """How many times memcheck saw a branch or an address depend on the secret.

    The shim (scripts/ct-shim.c) marks what the probe reads from the secret's file as undefined,
    its value unchanged; memcheck then reports every conditional jump and every memory access
    that depends on it.
    """
    run = subprocess.run(
        ["valgrind", "--tool=memcheck", "--error-limit=no", "-s", exe, secret_file],
        capture_output=True, text=True,
        env={**os.environ, **env, **ONE_THREAD, "VELT_CT_SECRET": secret_file,
             "LD_PRELOAD": shim})
    if run.returncode != 0:
        sys.exit(f"the probe failed under memcheck:\n{run.stdout}{run.stderr[-2000:]}")
    kinds = ("Conditional jump or move depends on uninitialised value",
             "Use of uninitialised value")
    total, lines = 0, run.stderr.split("\n")
    for i, line in enumerate(lines):
        found = re.search(r"== (\d+) errors in context", line)
        if found and i + 1 < len(lines) and any(k in lines[i + 1] for k in kinds):
            total += int(found.group(1))
    return total


def settle(run, target):
    """The count of one secret, and the target count: the probe's count for every secret.

    A test harness starts threads, and now and then one of them does more or less before the
    process ends: the same secret then counts some thousands more, or fewer. So the first
    secret's target is the most frequent of a few runs (the lowest of equals), and a later
    secret that differs from it is run again until a run equals it. Code that depends on the
    secret differs every time; such a secret's count is the most frequent of its runs.
    """
    if target is None:
        seen = [run() for _ in range(RETRIES)]
        return mode(seen), mode(seen)
    seen = [run()]
    while seen[-1] != target and len(seen) <= RETRIES:
        seen.append(run())
    return (target if seen[-1] == target else mode(seen)), target


def mode(values):
    """The most frequent of `values`, the lowest of equally frequent ones."""
    return min(values, key=lambda v: (-values.count(v), v))


def main():
    args = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    known = probes()
    args.add_argument("probes", nargs="*", help=f"which probes (default: all of {sorted(known)})")
    args.add_argument("--runs", type=int, default=40, help="secrets per probe (default 40)")
    args.add_argument("--seed", type=int, default=2392, help="seed of the random secrets")
    args.add_argument("--memcheck", action="store_true",
                      help="instead of counting: memcheck with the secret marked undefined")
    opts = args.parse_args()
    if not shutil.which("valgrind"):
        sys.exit("valgrind is needed")
    failed = False
    for name in opts.probes or sorted(known):
        if name not in known:
            sys.exit(f"no probe named {name}; there are {sorted(known)}")
        source, kind, secrets, allowed = known[name]
        with tempfile.TemporaryDirectory() as out:
            exe, env = build(source, kind, out)
            # One path for every run: its length is part of what the process does.
            secret_file = os.path.join(out, "secret.bin")
            if opts.memcheck:
                shim = build_shim(out)
                if shim is None:
                    sys.exit("--memcheck needs scripts/ct-shim.c built")
                worst = 0
                for secret in secrets(random.Random(opts.seed), min(opts.runs, 3))[:3]:
                    put(secret_file, secret)
                    worst = max(worst, reports(exe, env, secret_file, shim))
                if worst > allowed:
                    failed = True
                    print(f"{name}: SECRET-DEPENDENT: memcheck reports {worst:,} branches or "
                          f"addresses that depend on the secret ({allowed} are by design)")
                else:
                    print(f"{name}: memcheck reports {worst} (by design: {allowed})")
                continue
            open(stop_file(secret_file), "wb").close()
            shim = build_shim(out) if counts_regions() else None
            counts = {}
            target = None
            for secret in secrets(random.Random(opts.seed), opts.runs):
                secret = put(secret_file, secret)
                c, target = settle(lambda: count(exe, env, secret_file, shim), target)
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
                print(f"  {c:,}: {counts[c][0].hex()[:96]}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
