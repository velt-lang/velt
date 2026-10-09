//! The `velt dev` loop: build, start the program, watch the files the build read, and on a
//! change build the new version while the old one keeps running. A failed build prints its
//! diagnostics and leaves the old version serving; a good one replaces it.
//!
//! - JIT mode (default): the new version is a `velt dev --host` child that compiles and
//!   JIT-compiles the program itself, reports `built ok|failed` with the files it read over the
//!   dev channel and waits; only then is the old version stopped and the new one told to go. The
//!   supervisor does not compile at all, and the new version's compile overlaps the old one's
//!   serving. While a host runs, a change first goes to it over its reload channel: it swaps
//!   the changed code into the running program (state survives), or says why it must restart.
//!   After a restart (the same kind of edit likely follows), a new host starts together with
//!   the next request (quietly: the running host reports the build), so a restart doesn't wait
//!   for two front-end runs one after the other; it is ended unused after a swap. Not on
//!   Windows, where the second host costs more than it saves.
//! - `--exe` mode: the supervisor links an executable per version (`versions`), then swaps the
//!   processes.
//!
//! Interrupted (Ctrl-C, SIGTERM, SIGHUP; console events on Windows), the supervisor stops the
//! program the way a reload does, waits for it and exits with 128 + the signal (130 on Windows).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Instant, SystemTime};

use crate::cli::{DevArgs, DevMode, Emit};
use crate::commands::{build_options, failure_code, report};
use crate::driver::{self, Artifact, BuildError, OutputPaths, Session};

use velt_rt_host::dev::handover::reload::{request_reload, Reloaded};
use velt_rt_host::dev::handover::Stream;

use super::child::{Launch, Running};
use super::host::QUIET_ENV;
use super::interrupt;
use super::listeners::{Built, Handover};
use super::versions::Versions;
use super::watch::{Watcher, POLL};

/// Supervisor state for one `velt dev` session.
pub struct Supervisor {
    args: DevArgs,
    watcher: Watcher,
    running: Option<Running>,
    /// JIT mode: the running host's reload channel.
    channel: Option<Stream>,
    /// Environment every program version gets (the dev channel).
    env: Vec<(OsString, OsString)>,
    /// `--exe` mode: the version executables (set up by the first build).
    versions: Option<Versions>,
    /// The dev channel server (listener handover, build reports, stop channels).
    dev_socket: Handover,
    /// Build reports from JIT hosts.
    reports: Receiver<Built>,
    /// Whether the last reload ended in a restart: the next one likely does too (the same
    /// kind of edit again), so its host starts ahead of need.
    last_restarted: bool,
    /// The manifest's and lockfile's modification times when the last build started.
    manifest_seen: Vec<Option<SystemTime>>,
}

/// How one rebuild ended.
enum Outcome {
    /// The new version runs; the build read these files.
    Replaced(Vec<PathBuf>),
    /// The running host swapped in this many changed functions; the build read these files.
    Swapped(usize, Vec<PathBuf>),
    /// The running host could not take the new version (the reason), so it was restarted.
    Restarted(String, Vec<PathBuf>),
    /// The build failed; it read these files.
    Failed(Vec<PathBuf>),
}

impl Supervisor {
    /// Prepare a session (starts the dev channel server).
    pub fn new(args: DevArgs) -> Result<Supervisor, String> {
        let (tx, reports) = std::sync::mpsc::channel();
        let dev_socket = Handover::start(tx)?;
        let env = vec![(
            velt_rt_host::dev::SOCKET_ENV.into(),
            dev_socket.name().to_os_string(),
        )];
        interrupt::install();
        let cache = vpm::Locations::from_env().ok().map(|loc| loc.cache);
        let watcher = first_watcher(
            Watcher::default(),
            args.build.input.as_deref(),
            cache.as_deref(),
        );
        Ok(Supervisor {
            args,
            watcher,
            running: None,
            channel: None,
            env,
            versions: None,
            dev_socket,
            reports,
            manifest_seen: vec![],
            last_restarted: false,
        })
    }

    /// Run until the process is interrupted (Ctrl-C ends the supervisor and the program).
    pub fn run(mut self) -> ! {
        let mut since = Instant::now();
        let mut first = true;
        loop {
            self.rebuild(since, first);
            first = false;
            self.exit_if_interrupted();
            since = self.wait_for_change();
        }
    }

    /// Build the current sources; on success the new version replaces the running one.
    fn rebuild(&mut self, since: Instant, first: bool) {
        let snapshot = self.watcher.snapshot();
        let manifest_stamps = self.manifest_stamps();
        let outcome = match self.args.mode {
            DevMode::Exe => self.rebuild_exe(),
            DevMode::Jit | DevMode::Host => self.reload_jit(),
        };
        self.manifest_seen = manifest_stamps;
        let manifest = manifest_files(self.args.build.input.as_deref());
        let ms = since.elapsed().as_millis();
        match outcome {
            Outcome::Replaced(files) => {
                self.watcher
                    .set(files.into_iter().chain(manifest), &snapshot);
                let verb = if first { "started" } else { "reloaded" };
                eprintln!("velt dev: {verb} in {ms} ms");
            }
            Outcome::Swapped(functions, files) => {
                self.watcher
                    .set(files.into_iter().chain(manifest), &snapshot);
                let plural = if functions == 1 { "" } else { "s" };
                eprintln!("velt dev: hot-swapped {functions} function{plural} in {ms} ms");
            }
            Outcome::Restarted(reason, files) => {
                self.watcher
                    .set(files.into_iter().chain(manifest), &snapshot);
                eprintln!("velt dev: restarted ({reason}) in {ms} ms");
            }
            Outcome::Failed(files) => {
                self.watcher
                    .add(files.into_iter().chain(manifest), &snapshot);
                let still = if self.running.is_some() {
                    " (the previous version keeps running)"
                } else {
                    ""
                };
                eprintln!("velt dev: build failed{still}; waiting for changes");
            }
        }
    }

    /// JIT mode: let the running host take the new version in place if it can; otherwise
    /// replace it with a new host.
    fn reload_jit(&mut self) -> Outcome {
        let outcome = self.reload_jit_inner();
        match outcome {
            Outcome::Restarted(..) => self.last_restarted = true,
            Outcome::Swapped(..) | Outcome::Replaced(_) => self.last_restarted = false,
            Outcome::Failed(_) => {}
        }
        outcome
    }

    /// After a restart, the next host starts right away, in parallel with the request (where
    /// reports can be told apart by process id: a host given up on may still report later).
    /// After a swap it doesn't: a second front end beside the running host's would only slow
    /// the swap down.
    fn reload_jit_inner(&mut self) -> Outcome {
        let alive = self.running.as_mut().is_some_and(|r| r.exited().is_none());
        if !(alive && self.channel.is_some()) {
            return self.rebuild_jit(None);
        }
        // A host installs changed dependencies when it starts; one that may be killed unused
        // must not be in the middle of that.
        let manifest_changed = self.manifest_stamps() != self.manifest_seen;
        let spare = if SPECULATE && self.last_restarted && !manifest_changed {
            self.start_host(true).ok()
        } else {
            None
        };
        let reply = match &self.channel {
            Some(channel) => request_reload(channel),
            None => return self.rebuild_jit(spare),
        };
        match reply {
            Ok(Reloaded::Swapped { functions, files }) => {
                self.discard(spare);
                Outcome::Swapped(functions, files)
            }
            Ok(Reloaded::Failed { files }) => {
                self.discard(spare);
                Outcome::Failed(files)
            }
            Ok(Reloaded::Restart { reason, .. }) => match self.rebuild_jit(spare) {
                Outcome::Replaced(files) => Outcome::Restarted(reason, files),
                other => other,
            },
            // The host went away (crashed or exited meanwhile): start a new one.
            Err(_) => self.rebuild_jit(spare),
        }
    }

    /// The modification times of the manifest and lockfile.
    fn manifest_stamps(&self) -> Vec<Option<SystemTime>> {
        manifest_files(self.args.build.input.as_deref())
            .iter()
            .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
            .collect()
    }

    /// Start a JIT host (`quiet`: one that prints nothing about its build).
    fn start_host(&self, quiet: bool) -> Result<Running, String> {
        let program = std::env::current_exe().map_err(|e| format!("cannot locate velt: {e}"))?;
        let mut env = self.env.clone();
        if quiet {
            env.push((QUIET_ENV.into(), "1".into()));
        }
        Running::start(&Launch {
            program,
            args: host_args(&self.args),
            env,
            output: None,
        })
    }

    /// End a host that was started ahead of need and not needed after all.
    fn discard(&self, spare: Option<Running>) {
        if let Some(spare) = spare {
            spare.kill(&self.dev_socket);
        }
    }

    /// JIT mode: start a host (or take `spare`, a quiet one already started), wait for its
    /// build report, then replace the running one.
    fn rebuild_jit(&mut self, spare: Option<Running>) -> Outcome {
        let quiet = spare.is_some();
        let mut candidate = match spare.map_or_else(|| self.start_host(false), Ok) {
            Ok(candidate) => candidate,
            Err(msg) => {
                crate::style::error(&msg);
                return Outcome::Failed(vec![]);
            }
        };
        loop {
            if let Some(code) = interrupt::interrupted() {
                candidate.kill(&self.dev_socket);
                self.exit(code);
            }
            match self.reports.recv_timeout(POLL) {
                // A report from a host given up on earlier.
                Ok(built) if built.pid.is_some_and(|pid| pid != candidate.id()) => {}
                Ok(built) if built.ok => {
                    self.stop_running();
                    // A host that cannot receive `go` has died; its exit is reported later.
                    let _ = velt_rt_host::dev::handover::reply_go(&built.stream);
                    self.running = Some(candidate);
                    self.channel = Some(built.stream);
                    return Outcome::Replaced(built.files);
                }
                // A quiet host printed nothing: build again where the diagnostics show (the
                // sources changed since the running host built them).
                Ok(_) if quiet => {
                    candidate.kill(&self.dev_socket);
                    return self.rebuild_jit(None);
                }
                // The host printed the diagnostics and exits by itself.
                Ok(built) => return Outcome::Failed(built.files),
                Err(_) => {
                    if let Some(code) = candidate.exited() {
                        if quiet {
                            return self.rebuild_jit(None);
                        }
                        eprintln!(
                            "velt dev: the new version exited with code {code} before it was ready"
                        );
                        return Outcome::Failed(vec![]);
                    }
                }
            }
        }
    }

    /// `--exe` mode: build an executable, then swap the processes.
    fn rebuild_exe(&mut self) -> Outcome {
        let mut sess = Session::new();
        let launch = self.build_exe(&mut sess);
        report(&sess, self.args.build.verbose);
        let files: Vec<PathBuf> = sess.sm.files().map(|(_, f)| f.path.clone()).collect();
        let launch = match launch {
            Ok(launch) => launch,
            Err((err, output)) => {
                failure_code(&err);
                self.retire(output);
                return Outcome::Failed(files);
            }
        };
        self.stop_running();
        match Running::start(&launch) {
            Ok(running) => {
                self.running = Some(running);
                Outcome::Replaced(files)
            }
            Err(msg) => {
                crate::style::error(&msg);
                self.retire(launch.output);
                Outcome::Failed(files)
            }
        }
    }

    /// Link the next version; on failure also its output path (it may hold partial files).
    fn build_exe(&mut self, sess: &mut Session) -> Result<Launch, (BuildError, Option<PathBuf>)> {
        let mut opts =
            build_options(&self.args.build).map_err(|e| (BuildError::Failed(e), None))?;
        let target = opts.target();
        let default = OutputPaths::new(&opts.input, opts.output.as_deref(), Emit::Exe, &target);
        let versions = self.versions.get_or_insert_with(|| {
            let stem = default
                .executable
                .file_stem()
                .map_or_else(|| "program".into(), |s| s.to_string_lossy().into_owned());
            let dir = default.executable.parent().unwrap_or(Path::new("."));
            Versions::new(dir.join("dev"), stem)
        });
        let output = versions.next_output();
        opts.output = Some(output.clone());
        opts.emit = Emit::Exe;
        match driver::build(sess, &opts) {
            Ok(Artifact::Executable(exe)) => Ok(Launch {
                program: vpm::relpath::absolute(&exe),
                args: self.args.args.clone(),
                env: self.env.clone(),
                output: Some(output),
            }),
            Ok(other) => Err((
                BuildError::Ice(format!("dev build produced {other:?}")),
                Some(output),
            )),
            Err(err) => Err((err, Some(output))),
        }
    }

    fn stop_running(&mut self) {
        self.channel = None;
        if let Some(old) = self.running.take() {
            let output = old.stop(&self.dev_socket);
            self.retire(output);
        }
    }

    /// Delete a finished version's executable (now or on a later sweep).
    fn retire(&mut self, output: Option<PathBuf>) {
        if let (Some(versions), Some(output)) = (self.versions.as_mut(), output) {
            versions.retire(output);
        }
    }

    /// Wait until the watched files change; meanwhile report when the program exits by itself.
    fn wait_for_change(&mut self) -> Instant {
        loop {
            std::thread::sleep(POLL);
            self.exit_if_interrupted();
            if let Some(since) = self.watcher.poll() {
                return since;
            }
            if let Some(versions) = self.versions.as_mut().filter(|v| v.pending()) {
                versions.sweep();
            }
            // No host is starting now: a report is from one given up on (drop its connection).
            while self.reports.try_recv().is_ok() {}
            if let Some(code) = self.running.as_mut().and_then(Running::exited) {
                self.channel = None;
                if let Some(done) = self.running.take() {
                    let output = done.stop(&self.dev_socket);
                    self.retire(output);
                }
                eprintln!("velt dev: program exited with code {code}; waiting for changes");
            }
        }
    }

    /// Once interrupted: [`Supervisor::exit`].
    fn exit_if_interrupted(&mut self) {
        if let Some(code) = interrupt::interrupted() {
            self.exit(code);
        }
    }

    /// Stop the program (a stop request, then a kill after the grace period), wait for it,
    /// delete the version executables and exit with `code`.
    fn exit(&mut self, code: i32) -> ! {
        self.stop_running();
        for _ in 0..50 {
            match self.versions.as_mut() {
                Some(versions) if versions.pending() => versions.sweep(),
                _ => break,
            }
            std::thread::sleep(POLL);
        }
        std::process::exit(code)
    }
}

/// Whether a host may be started ahead of need here: build reports must carry the host's
/// process id, so a host given up on can be told apart. Not on Windows: there a second host
/// compiling beside the running one slowed its hot swaps about 4× and its restarts too (process
/// creation and the on-access scan cost more than the overlap saves).
const SPECULATE: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
));

/// Arguments for the JIT host child:
/// `dev --host [<file>] [--locked] [-v | --timings] -- <program args>`.
fn host_args(args: &DevArgs) -> Vec<OsString> {
    let mut out: Vec<OsString> = vec!["dev".into(), "--host".into()];
    out.extend(args.build.input.iter().map(|p| p.into()));
    if args.build.locked {
        out.push("--locked".into());
    }
    if args.build.timings {
        out.push("--timings".into());
    } else if args.build.verbose {
        out.push("-v".into());
    }
    out.push("--".into());
    out.extend(args.args.iter().cloned());
    out
}

/// `watcher` before the first build of `input`: it covers the program's directories, manifest
/// and lockfile, so the first build is compared with what they held when it started. Files in
/// the package `cache` are written once (`velt add`, `velt install`: checksum-verified registry
/// packages) and compared from when the first build reports them, so a package fetched just
/// before `velt dev` started doesn't count as saved during the build.
fn first_watcher(mut watcher: Watcher, input: Option<&Path>, cache: Option<&Path>) -> Watcher {
    watcher.seed(program_dirs(input));
    watcher.seed_files(manifest_files(input));
    if let Some(cache) = cache {
        watcher.written_once(cache);
    }
    watcher
}

/// The directories the program's own sources are in, watched from the first build on: the
/// entry file's directory, and in a package its root (with the manifest) and `src/`.
fn program_dirs(input: Option<&Path>) -> Vec<PathBuf> {
    let start = input
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut dirs = vec![start.clone()];
    if let Some(root) = vpm::manifest::find_package_root(&start) {
        dirs.push(root.join("src"));
        dirs.push(root);
    }
    dirs
}

/// The package manifest and lockfile, when the program is in a package.
fn manifest_files(input: Option<&Path>) -> Vec<PathBuf> {
    let start = input
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    match vpm::manifest::find_package_root(&start) {
        Some(root) => vec![
            root.join(vpm::manifest::MANIFEST_FILE),
            root.join(vpm::lockfile::LOCK_FILE),
        ],
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::super::watch::SETTLE;
    use super::*;
    use std::time::{Duration, Instant};

    /// The watcher reports no change for a while. It polls before each deadline check, so a
    /// descheduled thread still polls once after a change would have settled.
    fn stays_quiet(watcher: &mut Watcher) {
        let deadline = Instant::now() + SETTLE * 3;
        loop {
            assert!(watcher.poll().is_none(), "nothing changed");
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(POLL);
        }
    }

    /// The watcher reports a change within a few seconds.
    fn reports_a_change(watcher: &mut Watcher) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if watcher.poll().is_some() {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Every file under `dir`.
    fn files_in(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                files_in(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// A lockfile written just before `velt dev` started (`velt add`, `velt run`) was not saved
    /// during the first build: no second build follows it (#733: that build reported
    /// `hot-swapped 0 functions` after a later edit and left the old code running).
    #[test]
    fn a_recent_lockfile_needs_no_second_build() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(vpm::manifest::MANIFEST_FILE),
            "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };\n",
        )
        .unwrap();
        let main = root.join("main.vlt");
        std::fs::write(&main, "function main() {}\n").unwrap();
        std::fs::write(root.join(vpm::lockfile::LOCK_FILE), "{}\n").unwrap();
        let input = Some(main.as_path());
        let mut watcher = first_watcher(Watcher::new(false), input, None);
        let snapshot = watcher.snapshot();
        // What the first build read.
        let read = std::iter::once(main.clone()).chain(manifest_files(input));
        watcher.set(read, &snapshot);
        stays_quiet(&mut watcher);
    }

    /// `velt add util` and right away `velt dev`: the package fetched into the cache moments
    /// before the first build was not saved during it, so no second build follows. An edit to a
    /// cached file after the first build still counts.
    #[test]
    fn a_package_fetched_just_before_needs_no_second_build() {
        let dir = tempfile::tempdir().unwrap();
        let loc = vpm::Locations::under(&dir.path().join("home"));
        let util = vpm::scaffold::new_package(dir.path(), "util", true).unwrap();
        vpm::registry::publish_local(&util, &loc).unwrap();
        let app = vpm::scaffold::new_package(dir.path(), "app", false).unwrap();
        let spec = vpm::edit::DependencySpec {
            version: Some("0.1.0".into()),
            path: None,
        };
        // `velt add util`: the manifest, the lockfile and the package in the cache.
        vpm::edit::add_dependency(&app, "util", &spec).unwrap();
        vpm::install(&app, &loc, vpm::InstallOptions::default()).unwrap();
        let cached = loc.cache.join("util-0.1.0");
        let mut fetched = vec![];
        files_in(&cached, &mut fetched);
        assert!(!fetched.is_empty(), "nothing in {}", cached.display());

        // `velt dev`: the first build reads the program, the package and the manifest.
        let main = app.join("src").join("main.vlt");
        let input = Some(main.as_path());
        let first_build = |watcher: &mut Watcher| {
            let snapshot = watcher.snapshot();
            let read = std::iter::once(main.clone())
                .chain(fetched.iter().cloned())
                .chain(manifest_files(input));
            watcher.set(read, &snapshot);
        };
        let mut watcher = first_watcher(Watcher::new(false), input, Some(&loc.cache));
        first_build(&mut watcher);
        stays_quiet(&mut watcher);

        // An edit there afterwards (another length, so any clock sees it) is a change.
        let lib = fetched
            .iter()
            .find(|f| vpm::sources::is_source_file(f))
            .unwrap();
        std::fs::write(
            lib,
            "export function changed() {}\n// edited in the cache\n",
        )
        .unwrap();
        assert!(
            reports_a_change(&mut watcher),
            "edited after the first build"
        );

        // Without the cache known as written once, a second build follows (the files are
        // recent and the snapshot doesn't cover them): what this test guards against.
        let mut watcher = first_watcher(Watcher::new(false), input, None);
        first_build(&mut watcher);
        assert!(
            reports_a_change(&mut watcher),
            "recent files count as saved"
        );
    }
}
