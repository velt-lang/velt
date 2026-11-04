//! Asking a candidate clang what it is and whether it can compile for this machine, and resolving
//! its name the way starting it would.
//!
//! Starting clang costs about 70M instructions (Apple's, static) to 500M (Homebrew's, which loads
//! libLLVM), so the answers are kept across builds when they can be trusted: an answer is keyed by
//! the file's path, size and time, which only says something about the clang if that file is the
//! clang. A wrapper (ccache, asdf, mise, a script of one's own) stays the same while the clang
//! behind it changes, and the answer is what keys the objects, so a wrapper is asked each build:
//! see [`answers_for_itself`].
//!
//! A candidate that hangs costs [`PROBE_LIMIT`] each time it is asked, whether the build uses it or
//! not, so once it has failed to answer twice running a build's search leaves it alone for
//! [`HUNG_HOLD`] (see [`Waits`]).

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use velt_codegen_cl::CodeCache;

/// Oldest LLVM major version whose IR parser accepts what the backend emits.
pub(crate) const MIN_LLVM_MAJOR: u32 = 16;
/// Apple clang numbers releases on its own scheme; Apple clang 15 is based on LLVM 16.
const MIN_APPLE_CLANG_MAJOR: u32 = 15;
/// The clang every Mac with developer tools has, a shim that starts the active toolchain's clang.
pub(crate) const SYSTEM_CLANG: &str = "/usr/bin/clang";
/// How long a candidate gets to answer. Far longer than any clang takes (a first start on a busy
/// machine, a virus scan), so that only one that hangs is passed over.
pub(crate) const PROBE_LIMIT: Duration = Duration::from_secs(30);
/// How long, in seconds, a build's search leaves alone a candidate that hung, and how long one
/// timeout waits for a second. A hung shim then costs two waits an hour rather than one a build.
pub(crate) const HUNG_HOLD: u64 = 3600;
/// How much of a candidate's output is kept: its first line is all that is wanted. The rest is read
/// and dropped, so that it isn't killed by a closed pipe, whatever it writes.
const MOST_OUTPUT: u64 = 64 * 1024;
/// What the target check compiles. Its name is also how tests tell it from a real compile.
const SMALLEST_IR: &[u8] = b"define void @velt_clang_check() {\n  ret void\n}\n";
/// The memo's value for a check that passed; anything else is why it failed.
const COMPILES: &[u8] = b"ok";

/// Where the answers are kept across processes ([`set_probe_memo`]).
static PROBE_MEMO: OnceLock<&'static dyn CodeCache> = OnceLock::new();

/// Keep clang's answers in `memo` (veltc's build cache): every release build would otherwise pay
/// for them before compiling anything. Set once, before the first search; later calls are ignored.
pub fn set_probe_memo(memo: &'static dyn CodeCache) {
    let _ = PROBE_MEMO.set(memo);
}

fn memo() -> Option<&'static dyn CodeCache> {
    PROBE_MEMO.get().copied()
}

/// How long a candidate gets: [`PROBE_LIMIT`], or `$VELT_CLANG_PROBE_LIMIT_MS` for the tests that
/// run a hanging clang end to end.
pub(crate) fn probe_limit() -> Duration {
    (std::env::var("VELT_CLANG_PROBE_LIMIT_MS").ok())
        .and_then(|ms| ms.parse().ok())
        .map_or(PROBE_LIMIT, Duration::from_millis)
}

/// Whether this process asks a candidate that hung lately, and says nothing of the wait: `velt
/// doctor`, which is how a user finds out what is wrong and reports it itself.
static ASKS_EVERY_CLANG: AtomicBool = AtomicBool::new(false);

/// Ask every candidate in this process's search, even one that hung lately, and take back what is
/// known of one that answers now, for later builds too. Before the first search.
pub fn ask_every_clang() {
    ASKS_EVERY_CLANG.store(true, Ordering::Relaxed);
}

/// What a build's search needs to pay for a candidate that hangs only once in a while.
///
/// One timeout says little: macOS checks a new executable on its first start, which can take longer
/// than the limit when it has many to check (a `brew upgrade`, a busy machine), and the next start
/// is quick even if the first was killed. So the first timeout is only noted, and the next build
/// asks again. A second within [`HUNG_HOLD`] with no answer between them is what leaves the
/// candidate alone. Holding it after one would pass over a clang that works for the whole period.
///
/// Both are kept under the file's key. The cache has no way to replace or delete a value, so the
/// hour number is in the key, and the time in the value says exactly how old the note is: a note
/// counts from the moment it is written until [`HUNG_HOLD`] later, whichever of two hours it falls
/// in. A clock that goes back past the note makes it look written in the future, and such a note
/// doesn't count, so the clang is asked. A clock that jumps ahead only ends the wait early. Without
/// a clock, or without a cache (`VELT_BUILD_CACHE=off`), nothing is kept and each build waits, and
/// says so.
pub(crate) struct Waits<'a> {
    pub(crate) memo: Option<&'a dyn CodeCache>,
    pub(crate) limit: Duration,
    /// Seconds since 1970.
    pub(crate) now: Option<u64>,
    /// Says that a build waited.
    pub(crate) tell: &'a dyn Fn(&str),
}

/// What is known of a candidate hanging.
struct HungNote {
    /// There is a note, in force or not.
    seen: bool,
    /// It timed out once lately: another timeout leaves it alone.
    timed_out_once: bool,
    /// Leave the candidate alone.
    in_force: bool,
}

impl Waits<'_> {
    /// What `clang` says it is. If `hold`, a candidate that hung lately isn't asked.
    pub(crate) fn version(&self, clang: &Path, hold: bool) -> Probe {
        let note = self.hung_note(clang);
        if hold && note.in_force {
            return Probe::Held;
        }
        let probe = probe_with(clang, self.memo, self.limit);
        match probe {
            Probe::TimedOut => self.hung(clang, "`--version`"),
            _ if note.seen => self.answered(clang),
            _ => {}
        }
        probe
    }

    /// Whether `clang` compiles for this machine, as [`compiles_with`].
    pub(crate) fn check(&self, clang: &Path, banner: &str) -> Result<(), String> {
        let result = check_with(clang, banner, self.memo, self.limit);
        if matches!(result, Err(Failure::TimedOut(_))) {
            self.hung(clang, "a small compile");
        }
        result.map_err(Failure::into_text)
    }

    fn hung_note(&self, clang: &Path) -> HungNote {
        let mut note = HungNote {
            seen: false,
            timed_out_once: false,
            in_force: false,
        };
        let (Some(memo), Some(now)) = (self.memo, self.now) else {
            return note;
        };
        let Some(base) = hung_key(clang) else {
            return note;
        };
        let base = current(base, memo);
        for hour in [now / HUNG_HOLD, (now / HUNG_HOLD).saturating_sub(1)] {
            for kind in [ONCE, HELD] {
                let Some(value) = memo.get(&in_hour(&base, kind, hour)) else {
                    continue;
                };
                let Ok(written) = <[u8; 8]>::try_from(value.as_slice()) else {
                    continue;
                };
                note.seen = true;
                // `checked_sub` is `None` for a note from the future: the clock went back.
                let age = now.checked_sub(u64::from_le_bytes(written));
                let lately = age.is_some_and(|age| age < HUNG_HOLD);
                match kind {
                    ONCE => note.timed_out_once |= lately,
                    _ => note.in_force |= lately,
                }
            }
        }
        note
    }

    /// `clang` didn't answer `what` in time: keep that, and say the build waited.
    fn hung(&self, clang: &Path, what: &str) {
        if let (Some(memo), Some(now), Some(base)) = (self.memo, self.now, hung_key(clang)) {
            let kind = match self.hung_note(clang).timed_out_once {
                true => HELD,
                false => ONCE,
            };
            let base = current(base, memo);
            memo.put(&in_hour(&base, kind, now / HUNG_HOLD), &now.to_le_bytes());
        }
        let note = self.hung_note(clang);
        let then = if note.in_force {
            "it isn't asked again for an hour unless no other clang will do; `velt doctor` asks it \
             now"
        } else if note.timed_out_once {
            "the next build asks it again"
        } else {
            "`velt doctor` says which clang builds use"
        };
        (self.tell)(&format!(
            "note: this build waited {} s for `{}`, which did not answer {what}; {then}",
            self.limit.as_secs(),
            clang.display(),
        ));
    }

    /// `clang`, which a note says hung, did answer: later builds can ask it again.
    fn answered(&self, clang: &Path) {
        if let (Some(memo), Some(base)) = (self.memo, hung_key(clang)) {
            take_back(&base, memo);
        }
    }
}

/// The key of the notes that `clang` hung: the file's key and the name it is started as, since a
/// link to a program that picks what to run by that name (mise, asdf) is a clang per name.
fn hung_key(clang: &Path) -> Option<Vec<u8>> {
    let path = real_file(clang)?;
    let mut key = b"velt clang hung 1-".to_vec();
    key.extend(file_key(&path, &behind(&path))?);
    key.push(0);
    if let Some(name) = clang.file_name() {
        key.extend_from_slice(name.to_string_lossy().as_bytes());
    }
    Some(key)
}

/// The kinds of note: it timed out once, and it is left alone.
const ONCE: &str = "once";
const HELD: &str = "held";

fn in_hour(base: &[u8], kind: &str, hour: u64) -> Vec<u8> {
    let mut key = base.to_vec();
    key.extend_from_slice(format!("\0{kind} {hour}").as_bytes());
    key
}

/// [`Waits`] as a build has them. `velt doctor` reports for itself.
fn waits() -> Waits<'static> {
    let quiet = ASKS_EVERY_CLANG.load(Ordering::Relaxed);
    Waits {
        memo: memo(),
        limit: probe_limit(),
        now: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|t| t.as_secs()),
        tell: if quiet {
            &|_| {}
        } else {
            &|line| eprintln!("{line}")
        },
    }
}

/// Whether a clang candidate can compile the backend's IR, by what it says it is.
#[derive(Debug)]
pub(crate) enum Probe {
    Usable {
        /// The first line of its `--version`.
        banner: String,
        version: Option<ClangVersion>,
    },
    /// Runs, but too old; carries the version for the diagnostic (e.g. `clang 14.0.0`).
    TooOld(String),
    /// Started, but didn't answer within the limit.
    TimedOut,
    /// Not asked: it hung lately ([`Waits`]).
    Held,
    Missing,
}

/// What `clang` says it is, however it behaved before: `$VELT_CLANG`, which is final.
pub(crate) fn probe(clang: &Path) -> Probe {
    probe_with(clang, memo(), probe_limit())
}

/// What the candidate `clang` says it is, unless it hung lately.
pub(crate) fn probe_unless_hung(clang: &Path) -> Probe {
    waits().version(clang, !ASKS_EVERY_CLANG.load(Ordering::Relaxed))
}

/// What the candidate `clang` says it is, even if it hung lately: nothing else would do.
pub(crate) fn probe_anyway(clang: &Path) -> Probe {
    waits().version(clang, false)
}

pub(crate) fn probe_with(clang: &Path, memo: Option<&dyn CodeCache>, limit: Duration) -> Probe {
    let key = probe_key(clang).zip(memo).map(|(k, m)| current(k, m));
    let cached = (key.as_ref().zip(memo)).and_then(|(k, m)| m.get(k));
    if let Some(banner) = cached {
        return judge(&String::from_utf8_lossy(&banner));
    }
    let mut cmd = Command::new(clang);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match run_limited(cmd, limit) {
        Ran::Exited(status, stdout) if status.success() => {
            if let Some((k, m)) = key.as_ref().zip(memo) {
                m.put(k, &stdout);
            }
            judge(&String::from_utf8_lossy(&stdout))
        }
        Ran::TimedOut => Probe::TimedOut,
        Ran::Exited(..) | Ran::NotStarted(_) => Probe::Missing,
    }
}

/// Whether `clang`, which reported `banner`, can compile for this machine: `--version` also works
/// on a toolchain for another target only (wasi-sdk's `clang-NN`). Costs about as much as asking
/// its version, so it is asked of the clang about to be chosen, not of every candidate.
pub(crate) fn compiles(clang: &Path, banner: &str) -> Result<(), String> {
    waits().check(clang, banner)
}

#[cfg(test)]
pub(crate) fn compiles_with(
    clang: &Path,
    banner: &str,
    memo: Option<&dyn CodeCache>,
    limit: Duration,
) -> Result<(), String> {
    check_with(clang, banner, memo, limit).map_err(Failure::into_text)
}

fn check_with(
    clang: &Path,
    banner: &str,
    memo: Option<&dyn CodeCache>,
    limit: Duration,
) -> Result<(), Failure> {
    let key = check_key(clang, banner)
        .zip(memo)
        .map(|(k, m)| current(k, m));
    let cached = (key.as_ref().zip(memo)).and_then(|(k, m)| m.get(k));
    match cached.as_deref() {
        Some(COMPILES) => return Ok(()),
        Some(why) => return Err(Failure::Refused(String::from_utf8_lossy(why).into_owned())),
        None => {}
    }
    let target = format!("--target={}", velt_codegen_cl::host_triple());
    let args = [
        "-c",
        "-x",
        "ir",
        "-Wno-override-module",
        &target,
        "-",
        "-o",
        "-",
    ];
    let result = try_compile(clang, &args, limit);
    if let Some((k, m)) = key.as_ref().zip(memo) {
        match &result {
            Ok(()) => m.put(k, COMPILES),
            // Only clang's own verdict lasts: a crash, a file that couldn't be started because it
            // was being written, or a timeout may not happen next time.
            Err(Failure::Refused(why)) => m.put(k, why.as_bytes()),
            Err(Failure::Other(_) | Failure::TimedOut(_)) => {}
        }
    }
    result
}

/// Why a small compile failed.
#[derive(Debug)]
pub(crate) enum Failure {
    /// clang ran and said no.
    Refused(String),
    /// It didn't start, a wrapper couldn't start what it wraps, or it was killed.
    Other(String),
    /// It took too long.
    TimedOut(String),
}

impl Failure {
    pub(crate) fn into_text(self) -> String {
        match self {
            Failure::Refused(why) | Failure::Other(why) | Failure::TimedOut(why) => why,
        }
    }
}

/// Compile the smallest IR there is with `args`, which read stdin and write stdout.
pub(crate) fn try_compile(clang: &Path, args: &[&str], limit: Duration) -> Result<(), Failure> {
    let mut cmd = Command::new(clang);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match run_limited(cmd, limit) {
        Ran::Exited(status, _) if status.success() => Ok(()),
        Ran::Exited(status, stderr) => {
            let text = String::from_utf8_lossy(&stderr);
            let line = text.lines().find(|l| !l.trim().is_empty());
            let why = line.map_or_else(|| status.to_string(), |l| l.trim().to_string());
            Err(match status.code() {
                // A shell's "could not start that": a wrapper whose clang is being replaced.
                Some(126 | 127) | None => Failure::Other(why),
                Some(_) => Failure::Refused(why),
            })
        }
        Ran::NotStarted(why) => Err(Failure::Other(why)),
        Ran::TimedOut => Err(Failure::TimedOut(format!(
            "it took longer than {} s",
            limit.as_secs()
        ))),
    }
}

/// Take back what is known about `clang`, which turned out not to work: the file may be the same
/// while what it needs is gone (a library), so its answers would keep saying it works. This build
/// has gone wrong already, but the next one asks again and can pass it over.
pub(crate) fn forget(clang: &Path, banner: &str) {
    forget_in(clang, banner, memo());
}

pub(crate) fn forget_in(clang: &Path, banner: &str, memo: Option<&dyn CodeCache>) {
    let Some(memo) = memo else { return };
    for key in [probe_key(clang), check_key(clang, banner)]
        .into_iter()
        .flatten()
    {
        take_back(&key, memo);
    }
}

/// Leave `key`'s answer behind: see [`current`].
fn take_back(key: &[u8], memo: &dyn CodeCache) {
    memo.put(&taken_back(key, times_taken_back(key, memo)), b"");
}

/// `key` with what has been taken back from it. The cache keeps the first value stored under a key
/// for good, so an answer is taken back by leaving it behind: asking again stores the new answer
/// under another key.
fn current(mut key: Vec<u8>, memo: &dyn CodeCache) -> Vec<u8> {
    let times = times_taken_back(&key, memo);
    if times > 0 {
        key.extend_from_slice(format!("\0asked again {times}").as_bytes());
    }
    key
}

fn times_taken_back(key: &[u8], memo: &dyn CodeCache) -> u32 {
    (0..)
        .find(|&n| memo.get(&taken_back(key, n)).is_none())
        .unwrap_or(0)
}

/// The key of the note that `key`'s answer was taken back for the time numbered `times`.
fn taken_back(key: &[u8], times: u32) -> Vec<u8> {
    let mut key = key.to_vec();
    key.extend_from_slice(format!("\0taken back {times}").as_bytes());
    key
}

/// What came of running a program with a limit.
enum Ran {
    /// Its status, and what it wrote to the one pipe asked for (stdout, else stderr).
    Exited(ExitStatus, Vec<u8>),
    NotStarted(String),
    TimedOut,
}

/// Run `cmd`, which has stdout or stderr piped, and give it [`SMALLEST_IR`] if stdin is piped too.
/// A child that is still running at `limit` is killed; a process it started may live on, and the
/// thread reading their pipe with it, until this process exits.
fn run_limited(mut cmd: Command, limit: Duration) -> Ran {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Ran::NotStarted(e.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // It fits a pipe's buffer, so this doesn't block. One that exits unread is judged by that.
        let _ = stdin.write_all(SMALLEST_IR);
    }
    let pipe: Option<Box<dyn Read + Send>> = match (child.stdout.take(), child.stderr.take()) {
        (Some(out), _) => Some(Box::new(out)),
        (None, Some(err)) => Some(Box::new(err)),
        (None, None) => None,
    };
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = vec![];
        if let Some(mut pipe) = pipe {
            let _ = pipe.by_ref().take(MOST_OUTPUT).read_to_end(&mut text);
            let _ = std::io::copy(&mut pipe, &mut std::io::sink());
        }
        let _ = send.send(text);
    });
    let deadline = Instant::now() + limit;
    let finished = receive.recv_timeout(limit).ok().and_then(|text| {
        // The pipe is closed, so the child is done or nearly: `wait` could still block on one that
        // closed it and hangs.
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some((status, text)),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                _ => return None,
            }
        }
    });
    match finished {
        Some((status, text)) => Ran::Exited(status, text),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Ran::TimedOut
        }
    }
}

/// The memo key of `--version`'s answer for candidate `clang`: its resolved path (a bare name is
/// looked up on `PATH`), size and modification time. `None` when it can't be found or read, or
/// isn't the clang itself, so it is asked.
pub(crate) fn probe_key(clang: &Path) -> Option<Vec<u8>> {
    let path = real_file(clang)?;
    if !answers_for_itself(&path) {
        return None;
    }
    file_key(&path, &behind(&path))
}

/// The memo key of the target check of `clang`. Also kept for a wrapper, which is asked its version
/// each build anyway: that answer is in the key, and the check would be a second clang to start.
fn check_key(clang: &Path, banner: &str) -> Option<Vec<u8>> {
    let path = real_file(clang)?;
    let mut key = b"velt clang compiles 1-".to_vec();
    key.extend(file_key(&path, &behind(&path))?);
    key.extend_from_slice(banner.as_bytes());
    key.push(0);
    key.extend_from_slice(velt_codegen_cl::host_triple().as_bytes());
    Some(key)
}

fn real_file(clang: &Path) -> Option<PathBuf> {
    let path = resolve(clang)?;
    Some(path.canonicalize().unwrap_or(path))
}

fn behind(path: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") && path == Path::new(SYSTEM_CLANG) {
        developer_dirs()
    } else {
        vec![]
    }
}

/// The memo key of the file `path`, which starts the clang of whichever of `behind` is active.
pub(crate) fn file_key(path: &Path, behind: &[PathBuf]) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    let time = (meta.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())?
        .as_nanos();
    let mut key = b"velt clang --version 1-".to_vec();
    key.extend_from_slice(path.to_string_lossy().as_bytes());
    key.push(0);
    key.extend_from_slice(&meta.len().to_le_bytes());
    key.extend_from_slice(&time.to_le_bytes());
    for dir in behind {
        key.extend_from_slice(dir.to_string_lossy().as_bytes());
        key.push(0);
        for clang in APPLE_CLANGS {
            let meta = std::fs::metadata(dir.join(clang)).ok();
            let stamp = meta.map(|m| (m.len(), m.modified().ok()));
            key.extend_from_slice(format!("{stamp:?}").as_bytes());
        }
    }
    Some(key)
}

/// Whether the real file `path` (links followed) is a clang, so that what it answers lasts as long
/// as the file. It must be a native program: a script (asdf's shims, one's own) is
/// a wrapper. And it must be named clang: what a link to something else reaches (`ccache`, `mise`)
/// picks the program by the name it was started as. A compiled wrapper that is itself named
/// `clang…` can't be told from a clang, and is kept like one.
fn answers_for_itself(path: &Path) -> bool {
    named_clang(path) && is_native_program(path)
}

/// Whether the file's name says it is a clang: `clang`, `clang-23`, `clang.exe`.
pub(crate) fn named_clang(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("clang"))
}

fn is_native_program(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic));
    read.is_ok()
        && (matches!(
            magic,
            [0xcf, 0xfa, 0xed, 0xfe] // Mach-O, 64 bits
                | [0xce, 0xfa, 0xed, 0xfe] // Mach-O, 32 bits
                | [0xca, 0xfe, 0xba, 0xbe] // Mach-O, several architectures
                | [0x7f, b'E', b'L', b'F']
        ) || magic.starts_with(b"MZ")) // Windows
}

/// Where a developer directory keeps its clang: Xcode's, then the command line tools'.
const APPLE_CLANGS: [&str; 2] = [
    "Toolchains/XcodeDefault.xctoolchain/usr/bin/clang",
    "usr/bin/clang",
];

/// The developer directories `/usr/bin/clang` can start (`xcode-select`). An update of Xcode or of
/// the command line tools leaves the shim alone, so its own size and time would keep answering
/// with the old version. Asking `xcrun` would cost a process per build; looking at all of them
/// costs a few `stat`s and at worst one needless `--version`.
fn developer_dirs() -> Vec<PathBuf> {
    let selected = ["/var/db/xcode_select_link", "/var/select/developer_dir"]
        .iter()
        .filter_map(|link| std::fs::read_link(link).ok());
    let env = std::env::var_os("DEVELOPER_DIR").map(PathBuf::from);
    env.into_iter()
        .chain(selected)
        .chain(
            [
                "/Applications/Xcode.app/Contents/Developer",
                "/Library/Developer/CommandLineTools",
            ]
            .map(PathBuf::from),
        )
        .collect()
}

/// `clang` as a file, looked up on this process's `PATH`.
fn resolve(clang: &Path) -> Option<PathBuf> {
    resolve_in(clang, std::env::var_os("PATH").as_deref())
}

/// The file starting `clang` would start: itself when it has a directory part, else the first
/// match on `path` that can be started, since the search for a program skips those that can't.
pub(crate) fn resolve_in(clang: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    if clang.components().count() > 1 {
        return runnable(clang.to_path_buf());
    }
    std::env::split_paths(path?).find_map(|dir| runnable(dir.join(clang)))
}

/// `file` if it is a file this user can start, asked of the system: an execute bit may belong to
/// someone else.
#[cfg(unix)]
pub(crate) fn runnable(file: PathBuf) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(file.as_os_str().as_bytes()).ok()?;
    // SAFETY: `name` is a valid NUL-terminated string, alive for the call, which only reads it.
    let may_run = unsafe { libc::access(name.as_ptr(), libc::X_OK) } == 0;
    (may_run && file.is_file()).then_some(file)
}

/// `file` if it is there. Windows has no execute bit, and `Command` starts `clang` as `clang.exe`.
#[cfg(not(unix))]
pub(crate) fn runnable(file: PathBuf) -> Option<PathBuf> {
    let file = if file.extension().is_none() {
        file.with_extension("exe")
    } else {
        file
    };
    file.is_file().then_some(file)
}

/// What the output of `clang --version` says about using that clang.
pub(crate) fn judge(text: &str) -> Probe {
    let version = parse_version(text);
    match version {
        Some(v) if v.major < v.min_major() => Probe::TooOld(v.describe()),
        // An unrecognized version banner gets the benefit of the doubt.
        version => Probe::Usable {
            banner: text.lines().next().unwrap_or("").to_string(),
            version,
        },
    }
}

/// A clang version as printed by `clang --version`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClangVersion {
    pub(crate) apple: bool,
    pub(crate) major: u32,
    /// Major, minor and patch, to compare two clangs.
    pub(crate) number: (u32, u32, u32),
    full: String,
}

impl ClangVersion {
    pub(crate) fn min_major(&self) -> u32 {
        if self.apple {
            MIN_APPLE_CLANG_MAJOR
        } else {
            MIN_LLVM_MAJOR
        }
    }

    pub(crate) fn describe(&self) -> String {
        let vendor = if self.apple { "Apple clang" } else { "clang" };
        format!("{vendor} {}", self.full)
    }
}

/// Parse the first line of `clang --version`: `[Vendor ]clang version X.Y.Z[ (...)]`.
pub(crate) fn parse_version(text: &str) -> Option<ClangVersion> {
    let line = text.lines().next()?;
    let (prefix, rest) = line.split_once("clang version ")?;
    let full = rest.split_whitespace().next()?.to_string();
    let mut parts = full.split('.');
    let major = parts.next()?.parse().ok()?;
    // Distributions append to the last part: `14.0.0-1ubuntu1.1`, `20.1.0git`.
    let mut leading_number = || {
        let part = parts.next().unwrap_or("");
        let digits = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        part[..digits].parse().unwrap_or(0)
    };
    let number = (major, leading_number(), leading_number());
    Some(ClangVersion {
        apple: prefix.trim_end().ends_with("Apple"),
        major,
        number,
        full,
    })
}

#[cfg(test)]
#[path = "clang_probe_tests.rs"]
mod tests;
