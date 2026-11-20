# Linux builder: build Velt, join the crew, start a Claude agent

What a fresh Linux machine (Debian 12 or Ubuntu 24.04, x86_64 or arm64) needs to work as a crew builder for Velt, in order. Written 2026-11-20 from the Mac setup (mac-410d). The commands are the same on both architectures.

## 1. System packages

```sh
sudo apt-get update
sudo apt-get install -y build-essential clang-19 lld-19 git curl ca-certificates pkg-config \
  python3 procps unzip xz-utils
```

- **clang:** 16 or newer enables Velt's LLVM backend and the release-mode tests (CONTRIBUTING.md "Building"). If your distro has no `clang-19`, install `clang`/`lld` and check `clang --version` ≥ 16.
- **Docker image:** `scripts/linux/Dockerfile.bookworm` in the velt repo installs this same set.

## 2. Rust, nextest, Node, GitHub CLI

```sh
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy,rustfmt --default-toolchain stable
. "$HOME/.cargo/env"
cargo install cargo-nextest --locked          # check.sh runs only the selected tests, in parallel
```

- **Node 22 or newer:** the Node comparison in every PR self-check runs `.ts` files directly. Use nodejs.org or nvm. tsc comes from a local `npm i typescript` where needed.
- **GitHub CLI:** install `gh` (https://cli.github.com), then run `gh auth login` with the GitHub account the Mac sessions use. The crew repository (`andtii/crew`) is private, and agents push branches and open PRs with `gh`.
- **Git identity:** set the same author the Mac commits use. Copy `git config --global user.name` and `user.email` from the Mac; it is the GitHub noreply address. Commits keep this author, with no co-author trailers.

## 3. The Velt repository (worktree layout)

```sh
mkdir -p ~/dev/velt && cd ~/dev/velt
git clone https://github.com/velt-lang/velt.git main
cd main
cargo build --workspace
cargo run -p veltc --bin velt -- run tests/golden/m1/hello.vlt   # prints hello
cargo xtask install-hooks                                         # pre-push precheck, once per clone
```

- **Worktrees:** `~/dev/velt/main` stays on `main`. Each task gets its own worktree: `git worktree add ../branches/<task> -b <task> origin/main`. Use a build directory of its own: `CARGO_TARGET_DIR` per worktree, or the default `target/`.
  - **Do not share one target dir across worktrees.** Cargo reuses the other worktree's crates (a stale sema; test binaries with the other worktree's paths), and the `velt` binary finds the other worktree's `std/`.
  - If you must share one, set `VELT_STD=$PWD/std`, and run `cargo clean -p velt_common -p velt_sema -p velt_vir -p veltc` before building another worktree.
- **Disk:** a target dir is 6-20 GB. Keep 30 GB free before a heavy job.
- **Commands** (CLAUDE.md and CONTRIBUTING.md in the repo are the reference):
  - **Quality gate:** `scripts/check.sh` (it picks the checks from the files changed since `origin/main`).
  - **Goldens:** `VELT_GOLDEN=<filter> cargo test --workspace --test golden`.
  - **Before a push:** `cargo xtask precheck` (the hook runs it).

## 4. The released `velt`, to build crew

crew is written in Velt; its README asks for velt 0.6 or newer on `PATH`.

```sh
curl -fsSL https://github.com/velt-lang/velt/releases/latest/download/get-velt.sh | sh
velt --version
```

## 5. Claude Code

```sh
curl -fsSL https://claude.ai/install.sh | bash
claude            # log in with the subscription the sessions use; this creates ~/.claude
```

- **Run it once before step 6:** crew's install script adds its hooks to every existing Claude Code config dir (`~/.claude`, `~/.claude-*`).
- **Global preferences:** copy `~/.claude/CLAUDE.md` from the Mac. It holds the preferences every session follows: no co-author trailers, and no multi-line scripts inline in shell commands.

## 6. Join the crew

```sh
gh repo clone andtii/crew ~/dev/crew
cd ~/dev/crew
./install.sh --tags linux --hub 192.168.115.247:4747    # add ,bench if it should take measurement work
crew who                                                 # lists win-coordinator and the others
```

- **What `install.sh` does:**
  - builds crew with `velt build --release`;
  - links `~/.local/bin/crew` (put `~/.local/bin` on `PATH`);
  - installs the `/crew` command;
  - adds the SessionStart and SessionEnd hooks, so every Claude session joins as `<host>-<id>`;
  - writes `CREW_HUB` and `CREW_TAGS` to `~/.crew/env`.
- **The hub** runs on the coordinator's Windows machine at `192.168.115.247:4747`. It needs TCP 4747 reachable from the Linux box; without `--hub`, clients look for it by UDP broadcast on port 47470. No token is set (`CREW_TOKEN` is empty on the Mac).
- **Update later:** `git pull && ./install.sh` again.

## 7. Start the agent

```sh
cd ~/dev/velt/main
claude
```

Then give it the same opening the Mac sessions got:

> connect to crew and wait for tasks

The SessionStart hook prints the crew guide and the agent's name. The agent then:
- starts `crew watch` under its Monitor tool, and re-arms it every time it ends (about every 29 minutes);
- sets `crew status`;
- claims tasks posted for it (`--os linux`, or `--to <name>`).

For an unattended builder, start Claude with the permission mode you use on the other machines, so cargo, git and gh run without prompts.

## 8. Rules the crew runs by

These are learned on the Macs; tell the agent if it doesn't pick them up from the guide.

- **Heavy jobs** (builds, gates, benchmarks): wait for the coordinator's "go" addressed to you by name. Post `heavy start <name>: <what> (~min)` and `heavy done <name>` to all. One heavy job per machine at a time.
- **Never merge and never push to `main`.** The coordinator (win-coordinator) reviews and merges. CI starts only after the `ci:approved` label.
- **PRs:**
  - `Closes #N` or `Part of #N` in the description;
  - the self-check and gate result as the first comment;
  - no "Generated with" footers, session links or `Co-authored-by` lines;
  - commit messages and PR bodies passed as files (`git commit -F`, `--body-file`).
- **Every claim from a clean build of that worktree.** A new test must fail with the fix reverted; the reviewer checks.
- **Handoffs:** at a wind-down, commit and push everything (`wip/<name>/<topic>` for unfinished work). Then write `crew note <task> "HANDOFF: …"` or a file, and post `HANDOFF DONE <name>`.

## Where things are

- This file: `velt-lang/velt` branch `wip/mac-410d/943-spread`, `notes/mac-410d/linux-builder-setup.md`. Fetch it with `gh api repos/velt-lang/velt/contents/notes/mac-410d/linux-builder-setup.md?ref=wip/mac-410d/943-spread -H "Accept: application/vnd.github.raw"`.
- On the Mac it is also at `~/dev/velt/branches/handovers/linux-builder-setup.md`.
