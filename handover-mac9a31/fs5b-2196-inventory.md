#2196 step 5b (readdir, mkdir, rm): inventory and plan. Coding starts once #3218 merges: it shares rt_sched.velt / rt.velt / #fs-whole.

What the runtime does (velt_rt fs/ops.rs, the sync externs velt_rt_fs_{read_dir,read_dir_typed,mkdir,remove}_sync; velt_rt_wasm keeps its copies as in 5a):
- **readdir:** `fs::read_dir`, names lossy UTF-8, **sorted by bytes** (Node doesn't sort; the sort is Velt's for determinism, so I keep it). Typed: `d_type` (`DT_UNKNOWN` costs an `lstat`); Windows uses the find data. The error is `scandir 'path'`.
- **mkdir:** `create_dir` / `create_dir_all` (recursive: ok if it exists as a directory). The error is `mkdir 'path'`.
- **rm:** `lstat` first (`lstat 'path'`). Then a directory link or junction on Windows is `rmdir`, any other non-directory is `unlink`, and a directory is `rm` (recursive, `remove_dir_all`) or `rmdir`. Each error names the path.

Plan (one PR, POSIX and Windows):
- **POSIX:** `opendir`/`readdir`/`closedir` over `#fd-system`, with `struct dirent` offsets per target:
  - Linux (glibc 64-bit): `d_type` at 18, `d_name` at 19;
  - macOS arm64: `d_type` at 20, `d_name` at 21;
  - macOS x86-64: the 32-bit-inode `readdir` as with `fstat`: `d_type` at 6, `d_name` at 8.

  Checked against the headers (`velt native headers --std`, CHECKED). `mkdir(path, 0o777)`, `rmdir` and `unlink`; recursive `mkdir` walks up on ENOENT and accepts EEXIST for a directory; recursive `rm` walks the tree with the typed listing and never follows a link.
- **Windows:** `FindFirstFileExW(path\*, FindExInfoBasic, …, FIND_FIRST_EX_LARGE_FETCH)` and `FindNextFileW`, with "." and ".." skipped. A reparse point whose tag is symlink or mount point is a link. Also `CreateDirectoryW`, `RemoveDirectoryW` and `DeleteFileW`, and the `\\?\` paths of 5a.
- **The sort:** by code point (the byte order Rust used), not JS's UTF-16 order, so that a non-BMP name sorts as before.
- **Tests:**
  - goldens with Node's output: listing order made explicit (Node's is the OS order, so the Node twin sorts), typed kinds, a nested tree made and removed, every error, and a long path;
  - a velt_rt-free test that a link inside a tree being removed is removed as a link, its target kept;
  - a bench row: readdirSync of a 1,000-entry directory ×N, and mkdir + rm of a tree.

  Each new test gets a mutant.

Question: Node's `rmSync` without `force` throws ENOENT for a missing path, and the runtime's `remove` does too (`lstat 'x'`). The goldens guard their cleanup with `existsSync`, so I keep that. OK?

5b APPROVED (coordinator): keep rmSync ENOENT for a missing path + existsSync guards; keep code-point sort (document as determinism choice); dirent offsets checked against headers + a test that fails on a wrong d_type offset. Start when #3218 merges.
