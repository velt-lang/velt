#!/usr/bin/env python3
"""Moves Velt source files from `.vlt` to `.velt` (#2419), on main and on open branches.

    python3 velt-ext.py commit                  one commit on HEAD with the rename
    python3 velt-ext.py branch [--onto REF] [--stack | --squash]
                                                a branch's commits in the new naming, on REF
    python3 velt-ext.py verify [BRANCH...]       a moved branch still makes the same changes

An open branch runs the copy on main, since it may not have one:

    git fetch origin
    git show origin/main:scripts/velt-ext.py > "${TMPDIR:-/tmp}/velt-ext.py"
    python3 "${TMPDIR:-/tmp}/velt-ext.py" branch   # then: cargo xtask precheck, git push --force-with-lease

`branch` does not ask Git to detect thousands of renames. It rewrites the base and each commit of
the branch into the new naming, then rebases those commits onto REF (origin/main by default), where
every path already has its new name, so only real conflicts stop it: resolve them, `git rebase
--continue` as usual, then `verify`. The old tip is kept as refs/velt-ext/backup/<branch>, and is
never overwritten. `--stack` also moves local branches on the branch's own commits, for a stack run
from its top. A branch with merge commits has no history to replay: `--squash` makes it one commit.

A branch forked from another open branch whose own commits have moved since (a lower layer that was
rebased, or moved by this script on its own) is moved in two steps, since the lower layer's old
commits would otherwise come back as adds: `branch` as usual, then put its own N commits on the
moved lower branch with `git rebase --onto <lower> HEAD~N`. `--stack` says which local branches it
left alone because they no longer lie on this one.

It stops before changing anything if the target has no rename yet, if a file not tracked here would
be overwritten, or if the branch was moved before. At the end it compares the branch's changes on
the rewritten base and on the target, and fails with the way back when they differ.

The rewrite is a function from tree to tree: a second run changes nothing, and a line holding the
marker `velt-ext: keep` is left alone, for the code that reads the old extension on purpose.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile

SELF = {"scripts/velt-ext.py", "scripts/velt-ext-test.py"}
KEEP = b"velt-ext: keep"
TOKEN = re.compile(rb"(?<![A-Za-z0-9])vlt(?![A-Za-z0-9])")
TOKEN_UPPER = re.compile(rb"(?<![A-Za-z0-9])VLT(?![A-Za-z0-9])")
# Files where the plain rule would merge two names: (path, pattern, replacement), applied first.
SPECIAL = {
    # `vlt` and `velt` are both variables here, the source's path and the compiler's.
    "scripts/promise-order-diff.py": [(re.compile(rb"(?<![.\w\"'])vlt\b"), b"src_path")],
}


def git(*args, stdin=None, env=None, check=True):
    p = subprocess.run(["git", *args], input=stdin, capture_output=True, env=env)
    if check and p.returncode != 0:
        sys.exit(f"git {' '.join(args)}: {p.stderr.decode(errors='replace').strip()}")
    return p.stdout


def text(*args, **kw):
    return git(*args, **kw).decode().strip()


def rewrite_bytes(data):
    """`data` in the new naming; `data` itself when nothing changes."""
    if not (TOKEN.search(data) or TOKEN_UPPER.search(data)):
        return data
    lines = data.splitlines(keepends=True)
    for i, line in enumerate(lines):
        if KEEP not in line:
            lines[i] = TOKEN_UPPER.sub(b"VELT", TOKEN.sub(b"velt", line))
    return b"".join(lines)


def rewrite_path(path):
    return TOKEN.sub(b"velt", path.encode("utf-8", "surrogateescape")).decode(
        "utf-8", "surrogateescape"
    )


class Rewriter:
    """Maps trees to trees; blobs and trees already seen cost nothing, so a commit is cheap."""

    def __init__(self):
        self.cat = subprocess.Popen(
            ["git", "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
        )
        self.blobs = {}
        self.trees = {}

    def read(self, oid):
        self.cat.stdin.write(oid.encode() + b"\n")
        self.cat.stdin.flush()
        size = int(self.cat.stdout.readline().split()[2])
        data = self.cat.stdout.read(size)
        self.cat.stdout.read(1)
        return data

    def blob(self, oid, path):
        key = (oid, path) if path in SPECIAL else oid
        if key in self.blobs:
            return self.blobs[key]
        data = self.read(oid)
        new = data
        if path not in SELF and b"\0" not in data[:8192]:
            for pattern, replacement in SPECIAL.get(path, []):
                new = pattern.sub(replacement, new)
            new = rewrite_bytes(new)
        out = oid
        if new != data:
            out = text("hash-object", "-w", "--stdin", "--no-filters", stdin=new)
        self.blobs[key] = out
        return out

    def tree(self, tree_oid):
        if tree_oid in self.trees:
            return self.trees[tree_oid]
        entries, seen = [], {}
        for rec in git("ls-tree", "-r", "-z", tree_oid).split(b"\0"):
            if not rec:
                continue
            meta, raw = rec.split(b"\t", 1)
            mode, kind, oid = meta.decode().split()
            path = raw.decode("utf-8", "surrogateescape")
            new_path = rewrite_path(path)
            if new_path in seen and seen[new_path] != path:
                sys.exit(f"both {seen[new_path]} and {path} become {new_path}: remove one")
            seen[new_path] = path
            new_oid = self.blob(oid, path) if kind == "blob" else oid
            entries.append(
                f"{mode} {new_oid}\t".encode() + new_path.encode("utf-8", "surrogateescape") + b"\0"
            )
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, GIT_INDEX_FILE=os.path.join(d, "index"))
            git("update-index", "-z", "--index-info", stdin=b"".join(entries), env=env)
            out = text("write-tree", env=env)
        self.trees[tree_oid] = out
        return out

    def commit(self, oid, parents, message=None, tree_of=None):
        """`oid` with a rewritten tree (of `tree_of`, by default its own) and new parents; same
        authors and dates, so the same input always gives the same commit, which is what lets a
        stack's branches line up."""
        raw = git("cat-file", "commit", oid)
        head, _, body = raw.partition(b"\n\n")
        lines, skipping = [], False
        for line in head.split(b"\n"):
            if skipping and line.startswith(b" "):
                continue
            skipping = line.startswith(b"gpgsig")
            if skipping or line.startswith((b"tree ", b"parent ")):
                continue
            lines.append(line)
        tree = self.tree(text("rev-parse", f"{tree_of or oid}^{{tree}}"))
        top = [f"tree {tree}".encode()] + [f"parent {p}".encode() for p in parents]
        msg = body if message is None else message
        return text(
            "hash-object", "-t", "commit", "-w", "--stdin",
            stdin=b"\n".join(top + lines) + b"\n\n" + msg,
        )


def require_clean():
    if git("status", "--porcelain", "--untracked-files=no").strip():
        sys.exit("the working tree has changes: commit them first")


def cmd_commit(_):
    require_clean()
    head = text("rev-parse", "HEAD")
    old = text("rev-parse", "HEAD^{tree}")
    tree = Rewriter().tree(old)
    if tree == old:
        print("already in the new naming")
        return
    message = b"all: source files move from .vlt to .velt\n\nscripts/velt-ext.py commit (#2419).\n"
    new = text("commit-tree", tree, "-p", head, stdin=message)
    git("reset", "--hard", new)
    print(f"committed {new[:10]}")


def worktree_branches():
    out = text("worktree", "list", "--porcelain")
    return {l.split(" ", 1)[1] for l in out.splitlines() if l.startswith("branch ")}


def tracked_paths(rev):
    out = git("ls-tree", "-r", "-z", "--name-only", rev)
    return [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]


def has_old_name(path):
    return TOKEN.search(path.encode("utf-8", "surrogateescape")) is not None


def ref_exists(ref):
    return bool(text("rev-parse", "--verify", "-q", ref, check=False))


def refuse_overwrites(revs):
    """A path in `revs` that exists here without being tracked is someone's file: `reset --hard`
    would replace an untracked one silently, and ignored ones even a plain checkout."""
    tracked = {
        p.decode("utf-8", "surrogateescape") for p in git("ls-files", "-z").split(b"\0") if p
    }
    def in_the_way(path):
        if path not in tracked and os.path.lexists(path):
            return path
        # A file where the moved tree has a folder.
        parent = os.path.dirname(path)
        while parent:
            if parent not in tracked and os.path.lexists(parent) and not os.path.isdir(parent):
                return parent
            parent = os.path.dirname(parent)
        return None

    found = sorted({f for rev in revs for f in map(in_the_way, tracked_paths(rev)) if f})
    if found:
        sys.exit(
            f"{len(found)} file(s) here are not tracked and the moved branch has them, first "
            f"{found[0]}: move them away first. Nothing has changed."
        )


def changes(a, b):
    """What `a..b` changes, per file, without line numbers or context: what a rebase keeps when it
    moves a change across other people's commits. Before a file's first hunk `---` and `+++` are
    headers; after it they are content (`++i;`). Modes and binary patches count as changes."""
    out = git("diff", "--no-color", "--no-ext-diff", "--unified=0", "--no-renames", "--binary", a, b)
    result, current, header = {}, None, True
    for line in out.split(b"\n"):
        if line.startswith(b"diff --git "):
            current, header = line, True
            result[current] = []
        elif current is None:
            continue
        elif line.startswith(b"@@"):
            header = False
        elif header and line.startswith((b"--- ", b"+++ ", b"index ")):
            continue
        elif line:
            result[current].append(line)
    return {path: sorted(lines) for path, lines in result.items()}


def difference(branch, onto):
    """None when `branch` makes the changes it made before it moved; else what to tell the user."""
    head = f"refs/heads/{branch}"
    before = changes(f"refs/velt-ext/base/{branch}", f"refs/velt-ext/tip/{branch}")
    after = changes(text("merge-base", head, onto), head)
    if before == after:
        return None
    differ = [p.decode(errors="replace") for p in sorted(set(before) | set(after))
              if before.get(p) != after.get(p)]
    return (
        f"{branch}: the changes differ from before the move, in {len(differ)} file(s), first:\n"
        f"  {differ[0]}\n  the old tip: refs/velt-ext/backup/{branch}"
    )


def verify(branches, onto):
    """Exits naming each branch that no longer makes the changes it made before it moved."""
    problems = []
    for b in branches:
        found = difference(b, onto)
        print(f"{b}: {'the changes differ' if found else 'the same changes as before it moved'}")
        if found:
            problems.append(found)
    if problems:
        sys.exit("\n".join(problems) + "\nA conflict you resolved explains this; otherwise it is a bug.")


def forget(branch):
    """The command that drops what this script keeps for `branch`, so it can be moved again."""
    refs = " ".join(f"refs/velt-ext/{kind}/{branch}" for kind in ("backup", "base", "tip"))
    return f"git for-each-ref --format='delete %(refname)' {refs} | git update-ref --stdin"


def current_branch():
    branch = text("symbolic-ref", "--short", "-q", "HEAD", check=False)
    if not branch:
        sys.exit("HEAD is detached: check out the branch to move")
    return branch


def cmd_verify(args):
    branches = args.branches or [current_branch()]
    for branch in branches:
        if not ref_exists(f"refs/velt-ext/tip/{branch}"):
            sys.exit(f"{branch} was not moved by this script here")
    verify(branches, args.onto)


def cmd_branch(args):
    require_clean()
    os.chdir(text("rev-parse", "--show-toplevel"))
    branch = current_branch()
    if ref_exists("REBASE_HEAD") or os.path.exists(text("rev-parse", "--git-path", "rebase-merge")):
        sys.exit("a rebase is in progress: finish or abort it first")
    if not ref_exists(args.onto):
        sys.exit(f"{args.onto} does not exist: `git fetch origin` first")
    old = [p for p in tracked_paths(args.onto) if has_old_name(p)]
    if old:
        sys.exit(
            f"{args.onto} has no rename yet ({len(old)} files like {old[0]}): `git fetch origin`, "
            "or wait for it to merge. Nothing has changed."
        )
    head = text("rev-parse", "HEAD")
    base = text("merge-base", "HEAD", args.onto)
    order = text("rev-list", "--topo-order", "--reverse", "--parents", f"{base}..HEAD")
    commits = [line.split() for line in order.splitlines()]
    merges = [c[0] for c in commits if len(c) > 2]
    if merges and not args.squash:
        sys.exit(
            f"{len(merges)} merge commit(s) in {base[:10]}..HEAD, first {merges[0][:10]}: their "
            "changes cannot be replayed. Run again with --squash for one commit with the branch's "
            "changes, or rebase the branch yourself first. Nothing has changed."
        )
    if args.squash and args.stack:
        sys.exit("--squash leaves no commits for a stack's lower branches to point at")

    rewriter = Rewriter()
    if all(
        rewriter.tree(text("rev-parse", f"{c}^{{tree}}")) == text("rev-parse", f"{c}^{{tree}}")
        for c in [base] + [c[0] for c in commits]
    ):
        print(f"{branch}: already in the new naming")
        return
    backup = f"refs/velt-ext/backup/{branch}"
    if ref_exists(backup):
        sys.exit(
            f"{branch} was moved here before and has old names again. Its backup stays as it is: "
            f"to move it anyway, keep what you need, then {forget(branch)}"
        )

    new_base = rewriter.commit(base, [base], b"velt-ext: the base in the new naming\n")
    rewritten = {base: new_base}
    if args.squash:
        first = next(c[0] for c in commits if len(c) == 2) if len(merges) < len(commits) else head
        tip = rewriter.commit(first, [new_base], tree_of=head)
        print(f"{branch}: {len(commits)} commit(s) squashed into one")
    else:
        for oid, *parents in commits:
            rewritten[oid] = rewriter.commit(oid, [rewritten[p] for p in parents])
        tip = rewritten[head]
        print(f"{branch}: rewrote {len(commits)} commit(s)")
    refuse_overwrites([tip, args.onto])

    in_range = {c[0] for c in commits}
    lower = []
    if args.stack:
        for line in text("for-each-ref", "--format=%(objectname) %(refname:short)", "refs/heads").splitlines():
            oid, short = line.split(" ", 1)
            if short == branch or oid in in_range:
                continue
            fork = text("merge-base", oid, head, check=False)
            if fork in in_range and fork != head:  # Below this branch, not built on it.
                print(
                    f"  {short}: forks from this branch but has commits of its own since, not "
                    "moved: move it with `branch` from it, then rebase as the docstring says"
                )
        checked_out = worktree_branches()
        refs = text("for-each-ref", "--format=%(objectname) %(refname)", "refs/heads")
        for line in refs.splitlines():
            oid, ref = line.split(" ", 1)
            if ref == f"refs/heads/{branch}" or oid not in in_range:
                continue
            if ref in checked_out:
                print(f"  {ref}: checked out in another worktree, not moved")
                continue
            short = ref[len("refs/heads/") :]
            if ref_exists(f"refs/velt-ext/backup/{short}"):
                sys.exit(
                    f"{short} was moved here before: its backup stays. Nothing has changed. To "
                    f"move it anyway, keep what you need, then {forget(short)}"
                )
            lower.append((short, ref, oid))

    git("update-ref", backup, head)
    git("update-ref", f"refs/velt-ext/base/{branch}", new_base)
    git("update-ref", f"refs/velt-ext/tip/{branch}", tip)
    back = f"git rebase --abort; git reset --hard {backup}"
    for short, ref, oid in lower:
        git("update-ref", f"refs/velt-ext/backup/{short}", oid)
        git("update-ref", f"refs/velt-ext/base/{short}", new_base)
        git("update-ref", f"refs/velt-ext/tip/{short}", rewritten[oid])
        git("update-ref", ref, rewritten[oid])
        back += f" && git update-ref {ref} refs/velt-ext/backup/{short}"
        print(f"  {short} moves with it")
    moved = [branch] + [short for short, _, _ in lower]
    back += "".join(f" && {forget(b)}" for b in moved)

    git("reset", "-q", "--hard", tip)
    update_refs = ["--update-refs"] if lower else []
    p = subprocess.run(["git", "rebase", *update_refs, "--onto", args.onto, new_base])
    if p.returncode != 0:
        print(
            "\nThe rebase stopped on a real conflict: resolve it, `git rebase --continue`, then "
            f"`verify {' '.join(moved)}`. To go back:\n  {back}",
            file=sys.stderr,
        )
        sys.exit(p.returncode)
    verify(moved, args.onto)
    print("done: run `cargo xtask precheck` (the longer name can reflow a line), then push with "
          f"--force-with-lease. To go back:\n  {back.replace('git rebase --abort; ', '')}")


def main():
    parser = argparse.ArgumentParser(description="Move Velt sources from .vlt to .velt (#2419).")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("commit", help="one commit on HEAD with the rename").set_defaults(run=cmd_commit)
    br = sub.add_parser("branch", help="a branch's commits in the new naming, rebased")
    br.add_argument("--onto", default="origin/main")
    br.add_argument("--stack", action="store_true", help="also move local branches in the range")
    br.add_argument("--squash", action="store_true", help="one commit, for a branch with merges")
    br.set_defaults(run=cmd_branch)
    ver = sub.add_parser("verify", help="a moved branch still makes the same changes")
    ver.add_argument("branches", nargs="*", help="the moved branches (default: the current one)")
    ver.add_argument("--onto", default="origin/main")
    ver.set_defaults(run=cmd_verify)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
