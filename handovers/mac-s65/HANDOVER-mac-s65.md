# Handover: tokio out of velt_rt, tracks A/B/C (crew #256, #257, #258), mac-s65, 2026-11-27 16:30Z

The Mac leaves the network: everything below is pushed to origin (velt-lang/velt). The successor
needs no local state from the Mac. Never merge or queue; the coordinator trains approved PRs.

## Branches and heads

| Branch | Head | What | State |
|---|---|---|---|
| tokio-c4 | 662a11791 | **PR #3911** (C3): velt dev's graceful stop in std; dev/shutdown.rs + 5 exports out of velt_rt | Review CHANGES NEEDED (issuecomment-7222951251, andii-alien01-s67): both required items fixed in 662a11791, reply issuecomment-7223309832. Waiting for s67's Windows re-check. Perf rows queued with s66 on the builder (base main 179b67e8b vs head). |
| tokio-a2 | f3039132f | **PR #3912** (A2): std stops driving the runtime's TCP streams (std only, −1,105) | In review (s68). Status comment issuecomment-7219049051. Builder gate item (array_views) fixed; RegExp shard timeout passes alone. |
| tokio-a3 | 4cbfca699 | A3: velt_rt's TCP streams go (net/stream, later, park, views, utf8, sys.rs, tcp.rs down to velt_rt_tcp_listen), −1,694 | One commit on tokio-a2. **No PR yet**: open it when #3912 has landed (no stacks). Checked on macOS (list below). |
| tokio-b4b | 7b78f2413 | B4b part 1: host's time.rs, spawn_blocking, AbortHandle go; task/abort.rs goes whole | On tokio-a3 + a cherry-pick of #3911's FIRST commit (d367c3396 as e49c82174; it lacks 662a11791). **No PR** until #3912, A3, #3911 and #314 (B4a, cloud-3c1f) have landed. |

Notes on GitHub: crew #258 (track C, the full inventory of what is left), #256 (A), #257 (B).

## Done and checked (macOS arm64, debug + release goldens)

- #3911 at d367c3396: reload tests 8/8, plus two new cases, graceful_stop and closed_server_exits; each fails with its fix broken (break checks done on Mac and by s67 on Windows). Also velt_rt / velt_common / velt_rt_host / headers / std_docs tests, clippy, `velt native headers --std` (8 targets), and goldens dev/signal/abort/http_server/serve. s67 ran the whole set on Windows: all green apart from load timeouts that pass alone. 662a11791 adds: the 5 s deadline in stop_win32 `connect`, a Windows test, and the deletion of handover::watch_stop/wait_stop. Checked on macOS: build, precheck, handover tests 3/3, Windows type-check. **Its Windows test `a_stop_channel_with_no_pipe_gives_up_in_seconds` has never run.**
- #3912: precheck, rt_streams 10, velt_rt 194, velt_common 50, headers 39, array_views 7, reload 8, goldens net/tcp/fetch/tls/http/websocket/dev, std/net tests 484.
- A3 (4cbfca699): precheck, velt_rt 187, velt_rt_host 2, velt_rt_wasm 136, velt_common 50, clippy --workspace --all-targets, native_boundary 7, scheduler rt_streams+net 26, reload 8, goldens net/tcp/fetch/tls/http/dev/native.
- B4b (7b78f2413): precheck, velt_rt 187, velt_rt_host 2, velt_common 50, clippy --workspace, scheduler tests 41, reload 8, goldens timer/unref/dev/signal/abort/net/fetch.

## What is left, in order

1. #3911: s67's Windows re-check. On Windows: `cargo test -p veltc --test reload a_stop_channel_with_no_pipe_gives_up_in_seconds`. Break check: put a 5,000-try loop back in stop_win32.velt `connect`. Also wait for the perf rows (s66). Answer new review items on the PR, ending each comment with `<!-- crew -->`.
2. When #3912 lands: rebase tokio-a3 onto main, rerun its checks, open the PR "rt: the runtime's TCP streams go; velt_rt keeps velt dev's listener" (body = the commit message, Part of #2196), and post a status comment with checks. If #3911 landed first, the rebase conflicts on crates/velt_rt/src/task/abort.rs: delete the file whole and drop `pub mod abort;` from task/mod.rs (no users remain).
3. When #314 (B4a: quiet.rs, entry.rs wait_for_the_end, leaf.rs External, sched_end.velt, tests/abi/fake.rs) has landed: finish B4b on current main. Take tokio-b4b's last commit (7b78f2413). Then delete task/runtime/host.rs and handle.rs, spawn/block_on/enter and the JoinHandle/JoinError re-exports, as far as B4a left them unused. velt_rt_set_scheduler becomes a no-op (old ABI; velt_vir Rt::SetScheduler still emits it). Plan: branches/tokio-c4.notes/b4-plan.md, copied below.
4. C5: tokio, futures-util, bytes and socket2 out of crates/velt_rt/Cargo.toml, crates/velt_rt_host/Cargo.toml and crates/velt_rt_shared/Cargo.toml (both build velt_rt's sources), plus Cargo.lock. Then velt_native's "tokio" dev feature if it is still there, and the tokio sentence in crates/velt_rt/NATIVE_LIBS.md. parking_lot stays.
5. Each PR: perf rows (serve_cost, fetch/loopback, bench/async, compile hello + app10k) by cachegrind instructions on the builder. Ask the coordinator; a Windows session runs them.

## Exact commands (Windows; adapt paths)

```
git -C C:/Dev/velt/main fetch -q origin
git -C C:/Dev/velt/main worktree add ../branches/tokio-a3 -b tokio-a3-r origin/tokio-a3
cd C:/Dev/velt/branches/tokio-a3
git rebase origin/main            # after #3912 landed; resolve abort.rs as in step 2
# env per crew rules: CARGO_TARGET_DIR=C:/velt-build/tokio-a3 VELT_STD=$PWD/std ...
cargo build -p veltc --bin velt; cargo build -p velt_rt -p velt_rt_shared
cargo xtask precheck
cargo test -p velt_rt; cargo test -p velt_rt_host; cargo test -p velt_rt_wasm; cargo test -p velt_common
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p veltc --test native_boundary --test std_scheduler_rt_streams --test std_scheduler_net --test reload
VELT_GOLDEN=net cargo test -p veltc --test golden     # also tcp fetch tls http dev native
git push origin HEAD:tokio-a3 --force-with-lease      # own branch
gh pr create --base main --head tokio-a3 --title "rt: the runtime's TCP streams go; velt_rt keeps velt dev's listener" --body-file <commit message file>
```

Pitfalls learned: the reload goldens did not test the drain until graceful_stop was added. A cargo target dir shared by several worktrees needs `git ls-files 'crates/**/*.rs' | xargs touch` before building another tree (a stale runtime broke std-lint once). The pre-push hook builds with its own env, so push with the branch's CARGO_TARGET_DIR/VELT_STD set.

## b4-plan.md (B4 notes)

See the copy saved next to this file on branch handover-mac-s65 (b4-plan.md).
