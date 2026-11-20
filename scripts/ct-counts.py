#!/usr/bin/env python3
"""Whether a probe executes the same number of instructions whatever its secret.

A probe (bench/crypto/ct/*.velt, or a white-box test file in std for what programs cannot
import) reads a secret from the file it is given and runs the code under test on it. This builds
the probe in release mode and runs it under valgrind's cachegrind once per secret: edge values
and random ones. Constant-time code executes one count; a second count means some branch or loop
depends on the secret, in the machine code if not in the source (LLVM turns a mask it can see
through into a branch).

What this cannot see: a memory address that depends on the secret with no change in the count.
The memcheck command in std/crypto/field_p256.velt is the check for that.

  scripts/ct-counts.py p384            # one probe
  scripts/ct-counts.py                 # all of them
  scripts/ct-counts.py --runs 70 p384

Needs valgrind and a built `velt` (target/debug/velt, or $VELT). Exit status 1 when a probe has
more than one count.
"""
import argparse, json, os, random, re, shutil, subprocess, sys, tempfile

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


# name: (source relative to the repository, how it is built, the secrets)
PROBES = {
    "p384": ("bench/crypto/ct/p384.velt", "program", scalars384),
}


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
    args.add_argument("probes", nargs="*", help=f"which probes (default: all of {sorted(PROBES)})")
    args.add_argument("--runs", type=int, default=40, help="secrets per probe (default 40)")
    args.add_argument("--seed", type=int, default=2392, help="seed of the random secrets")
    opts = args.parse_args()
    if not shutil.which("valgrind"):
        sys.exit("valgrind is needed")
    failed = False
    for name in opts.probes or sorted(PROBES):
        if name not in PROBES:
            sys.exit(f"no probe named {name}; there are {sorted(PROBES)}")
        source, kind, secrets = PROBES[name]
        with tempfile.TemporaryDirectory() as out:
            exe, env = build(source, kind, out)
            # One path for every run: its length is part of what the process does.
            secret_file = os.path.join(out, "secret.bin")
            counts = {}
            for secret in secrets(random.Random(opts.seed), opts.runs):
                with open(secret_file, "wb") as f:
                    f.write(secret)
                counts.setdefault(count(exe, env, secret_file), []).append(secret)
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
