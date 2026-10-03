//! The registry commands: `velt registry serve` (share a registry directory over HTTP, crate
//! `velt_registry`), `velt registry user` (its users and their tokens), and `velt yank`,
//! `velt owner` and `velt search` against the current package's registry.

use std::net::TcpListener;
use std::path::PathBuf;

use crate::cli::registry::{OwnerAction, UserAction};
use crate::cli::RegistryArgs;
use crate::style;

fn registry_dir(dir: Option<&PathBuf>) -> Result<PathBuf, String> {
    match dir {
        Some(d) => Ok(d.clone()),
        None => Ok(vpm::Locations::from_env()?.registry),
    }
}

/// Serve until interrupted.
pub fn serve_command(args: &RegistryArgs) -> Result<(), String> {
    let root = registry_dir(args.dir.as_ref())?;
    std::fs::create_dir_all(&root)
        .map_err(|e| format!("cannot create `{}`: {e}", root.display()))?;
    velt_registry::check_dir(&root)?;
    let open = velt_registry::auth::is_open(&root)?;
    if open && std::env::var_os(vpm::remote::TOKEN_VAR).is_some_and(|t| !t.is_empty()) {
        return Err(format!(
            "${} no longer protects a registry server, and `{}` has no users, so it would accept uploads from anyone; add a user with `velt registry user add <name> --dir {}` (each user gets their own token), then start the server again",
            vpm::remote::TOKEN_VAR,
            root.display(),
            root.display()
        ));
    }
    let users = velt_registry::auth::user_names(&root)?;
    let listener = TcpListener::bind(&args.addr)
        .map_err(|e| format!("cannot listen on {}: {e}", args.addr))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let writes = if open {
        "open: anyone may publish; `velt registry user add <name>` requires tokens".to_string()
    } else {
        format!("{} users; writes need a user's token", users.len())
    };
    eprintln!(
        "velt registry: serving {} at http://{addr} ({writes})",
        root.display()
    );
    let registry = velt_registry::Registry { root };
    velt_http::serve(&listener, registry.handler(), velt_registry::MAX_ARCHIVE);
    Ok(())
}

/// `velt registry user add|remove|token <name>`: new tokens go to stdout, once.
pub fn user_command(action: UserAction, name: &str, dir: Option<&PathBuf>) -> Result<(), String> {
    let root = registry_dir(dir)?;
    let token = match action {
        UserAction::Add => velt_registry::auth::add_user(&root, name)?,
        UserAction::Token => velt_registry::auth::rotate_token(&root, name)?,
        UserAction::Remove { open } => {
            let removed = velt_registry::auth::remove_user(&root, name, open)?;
            style::status("Removed", &format!("user `{name}`"));
            if !removed.owned.is_empty() {
                style::status(
                    "Removed",
                    &format!("`{name}` as an owner of {}", removed.owned.join(", ")),
                );
            }
            if !removed.unowned.is_empty() {
                style::warning(&format!(
                    "{} now {} no owner: assign one with `velt registry owner add <package> <user>`",
                    removed.unowned.join(", "),
                    if removed.unowned.len() == 1 { "has" } else { "have" }
                ));
            }
            return Ok(());
        }
    };
    let verb = if action == UserAction::Add {
        "Added"
    } else {
        "Replaced"
    };
    style::status(
        verb,
        &format!(
            "the token of `{name}` (shown once; the user stores it with `velt login <registry-url>`, or sets ${} in CI)",
            vpm::remote::TOKEN_VAR
        ),
    );
    println!("{token}");
    Ok(())
}

/// `velt registry owner add|remove <pkg> <user>`: an administrator's change, made directly in
/// the registry directory (assigns owners to packages that have none).
pub fn admin_owner_command(
    add: bool,
    package: &str,
    user: &str,
    dir: Option<&PathBuf>,
) -> Result<(), String> {
    let root = registry_dir(dir)?;
    let owners = velt_registry::owners::set_by_admin(&root, package, user, add)?;
    let shown = if owners.is_empty() {
        "none".to_string()
    } else {
        owners.join(", ")
    };
    style::status("Owners", &format!("of `{package}`: {shown}"));
    Ok(())
}

/// The registry of the package around the current directory, else `$VELT_REGISTRY`'s.
fn locations() -> Result<vpm::Locations, String> {
    let loc = vpm::Locations::from_env()?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    match vpm::manifest::find_package_root(&cwd) {
        Some(root) => Ok(loc.with_manifest(&vpm::Manifest::from_dir(&root)?)),
        None => Ok(loc),
    }
}

/// `velt yank <pkg>@<version> [--undo]`.
pub fn yank_command(name: &str, version: &str, undo: bool) -> Result<(), String> {
    let loc = locations()?;
    vpm::yank::yank(&loc, name, version, !undo)?;
    let verb = if undo { "Unyanked" } else { "Yanked" };
    style::status(verb, &format!("`{name}` {version} ({})", loc.describe()));
    Ok(())
}

/// `velt owner list|add|remove <pkg> [<user>]` (registry servers only).
pub fn owner_command(action: &OwnerAction, package: &str) -> Result<(), String> {
    let loc = locations()?;
    let Some(url) = &loc.remote else {
        return Err(format!(
            "packages have owners only on a registry server; `{}` is a local registry (set `registry` in package.vlt or $VELT_REGISTRY to a URL)",
            loc.describe()
        ));
    };
    match action {
        OwnerAction::List => {
            for owner in vpm::remote::owners(url, package)? {
                println!("{owner}");
            }
        }
        OwnerAction::Add(user) | OwnerAction::Remove(user) => {
            let add = matches!(action, OwnerAction::Add(_));
            vpm::remote::set_owner(url, package, user, add)?;
            let verb = if add { "Added" } else { "Removed" };
            style::status(verb, &format!("`{user}` as an owner of `{package}`"));
        }
    }
    Ok(())
}

/// `velt login <url>`: read the token from stdin (one line; a prompt when it is a terminal) and
/// store it for that registry.
pub fn login_command(url: &str) -> Result<(), String> {
    use std::io::{BufRead, IsTerminal};
    let key = vpm::credentials::registry_key(url)?;
    vpm::credentials::check_transport(&key, "a registry token")?;
    let stdin = std::io::stdin();
    let mut token = String::new();
    let read = if stdin.is_terminal() {
        eprint!("Token for {key}: ");
        // The token is not shown while it is typed (or pasted); the guard restores the
        // terminal on every path.
        let _quiet = NoEcho::start();
        let read = stdin.lock().read_line(&mut token);
        eprintln!();
        read
    } else {
        stdin.lock().read_line(&mut token)
    };
    read.map_err(|e| format!("cannot read the token from stdin: {e}"))?;
    let path = vpm::credentials::default_path()?;
    vpm::credentials::login(&path, &key, &token)?;
    style::status(
        "Saved",
        &format!("the token for {key} in {}", path.display()),
    );
    Ok(())
}

/// Turns off the terminal's echo of stdin until dropped (nothing when that fails). An interrupt
/// while the token is typed (Ctrl+C, a closed terminal) restores the echo before the process
/// ends, so the shell isn't left without it.
struct NoEcho {
    on: bool,
}

impl NoEcho {
    fn start() -> NoEcho {
        NoEcho {
            on: no_echo::start(),
        }
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        if self.on {
            no_echo::stop();
        }
    }
}

#[cfg(unix)]
mod no_echo {
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The terminal attributes before echo was turned off, and the signal actions replaced.
    struct Saved {
        termios: UnsafeCell<std::mem::MaybeUninit<libc::termios>>,
        actions: UnsafeCell<[std::mem::MaybeUninit<libc::sigaction>; 3]>,
    }
    // SAFETY: written once in `start` before SAVED is set, read only while it is set.
    unsafe impl Sync for Saved {}
    static STATE: Saved = Saved {
        termios: UnsafeCell::new(std::mem::MaybeUninit::uninit()),
        actions: UnsafeCell::new([std::mem::MaybeUninit::uninit(); 3]),
    };
    static SAVED: AtomicBool = AtomicBool::new(false);
    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    /// Restores the terminal, then lets the signal end the process as it would have.
    extern "C" fn on_signal(signal: libc::c_int) {
        // SAFETY: tcsetattr, signal and raise are async-signal-safe; the attributes were saved
        // before the handler was installed.
        unsafe {
            if SAVED.load(Ordering::SeqCst) {
                libc::tcsetattr(
                    libc::STDIN_FILENO,
                    libc::TCSANOW,
                    (*STATE.termios.get()).as_ptr(),
                );
            }
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }

    pub fn start() -> bool {
        // SAFETY: tcgetattr/tcsetattr on stdin and sigaction with structures this module owns;
        // STATE is written before SAVED publishes it.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return false;
            }
            (*STATE.termios.get()).write(t);
            SAVED.store(true, Ordering::SeqCst);
            let actions = &mut *STATE.actions.get();
            for (i, signal) in SIGNALS.into_iter().enumerate() {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, actions[i].as_mut_ptr());
            }
            t.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) != 0 {
                stop();
                return false;
            }
            true
        }
    }

    pub fn stop() {
        // SAFETY: restores what `start` saved, then the previous signal actions.
        unsafe {
            if !SAVED.swap(false, Ordering::SeqCst) {
                return;
            }
            libc::tcsetattr(
                libc::STDIN_FILENO,
                libc::TCSANOW,
                (*STATE.termios.get()).as_ptr(),
            );
            let actions = &*STATE.actions.get();
            for (i, signal) in SIGNALS.into_iter().enumerate() {
                libc::sigaction(signal, actions[i].as_ptr(), std::ptr::null_mut());
            }
        }
    }
}

#[cfg(windows)]
mod no_echo {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleCtrlHandler, SetConsoleMode, ENABLE_ECHO_INPUT,
        STD_INPUT_HANDLE,
    };

    /// The console mode before echo was turned off.
    static MODE: AtomicU32 = AtomicU32::new(0);
    static SAVED: AtomicBool = AtomicBool::new(false);

    /// Ctrl+C, Ctrl+Break or a closed console: restore the mode, then let the default handler
    /// end the process.
    unsafe extern "system" fn on_event(_event: u32) -> BOOL {
        if SAVED.load(Ordering::SeqCst) {
            // SAFETY: restores the mode `start` read from this process's stdin.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), MODE.load(Ordering::SeqCst));
            }
        }
        0
    }

    pub fn start() -> bool {
        // SAFETY: console calls on the process's own stdin handle.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return false;
            }
            MODE.store(mode, Ordering::SeqCst);
            SAVED.store(true, Ordering::SeqCst);
            SetConsoleCtrlHandler(Some(on_event), 1);
            if SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) == 0 {
                stop();
                return false;
            }
            true
        }
    }

    pub fn stop() {
        if !SAVED.swap(false, Ordering::SeqCst) {
            return;
        }
        // SAFETY: restores the mode `start` read, then removes the handler it added.
        unsafe {
            SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), MODE.load(Ordering::SeqCst));
            SetConsoleCtrlHandler(Some(on_event), 0);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod no_echo {
    pub fn start() -> bool {
        false
    }
    pub fn stop() {}
}

/// `velt logout <url>`: forget the stored token of that registry.
pub fn logout_command(url: &str) -> Result<(), String> {
    let key = vpm::credentials::registry_key(url)?;
    let path = vpm::credentials::default_path()?;
    if vpm::credentials::logout(&path, &key)? {
        style::status("Removed", &format!("the token for {key}"));
    } else {
        style::status("Unchanged", &format!("no token stored for {key}"));
    }
    Ok(())
}

/// `velt search <text>`: `name  version  description` lines on stdout (the description cut to
/// the terminal's width), or with `--json` the registry's answer as one JSON document.
pub fn search_command(query: &str, json: bool) -> Result<(), String> {
    let loc = locations()?;
    let hits = vpm::search::search(&loc, query)?;
    if json {
        println!("{}", vpm::search::to_json(&hits));
        return Ok(());
    }
    if hits.is_empty() {
        style::status(
            "Searched",
            &format!("{}: no package matches `{query}`", loc.describe()),
        );
    }
    let name_width = hits.iter().map(|h| h.name.len()).max().unwrap_or(0);
    let version_width = hits.iter().map(|h| h.version.len()).max().unwrap_or(0);
    let columns = terminal_columns();
    for hit in hits {
        let line = format!("{:name_width$}  {:version_width$}", hit.name, hit.version);
        let left = columns.map(|c| c.saturating_sub(line.chars().count() + 2));
        match hit.description.as_deref().map(|d| fit(d, left)) {
            Some(d) if !d.is_empty() => println!("{line}  {d}"),
            _ => println!("{}", line.trim_end()),
        }
    }
    Ok(())
}

/// The terminal's width when stdout is one (`$COLUMNS` overrides it; 100 when it can't be
/// asked); `None` when stdout is piped (nothing is cut then).
fn terminal_columns() -> Option<usize> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let env = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok());
    Some(env.or_else(asked_columns).unwrap_or(100))
}

#[cfg(unix)]
fn asked_columns() -> Option<usize> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes one `winsize` into `size`, which outlives the call.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    (ok && size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(windows)]
fn asked_columns() -> Option<usize> {
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };
    // SAFETY: the handle is the process's own stdout; the call fills `info`, which outlives it.
    unsafe {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) == 0 {
            return None;
        }
        let columns = info.srWindow.Right - info.srWindow.Left + 1;
        (columns > 0).then_some(columns as usize)
    }
}

#[cfg(not(any(unix, windows)))]
fn asked_columns() -> Option<usize> {
    None
}

/// Fewest columns worth showing a description in.
const MIN_DESCRIPTION: usize = 10;

/// `text` cut to `width` display columns with a `…` when it is longer (`None`: no limit); empty
/// when fewer than [`MIN_DESCRIPTION`] columns are left.
fn fit(text: &str, width: Option<usize>) -> String {
    let Some(width) = width else {
        return text.to_string();
    };
    if width < MIN_DESCRIPTION {
        return String::new();
    }
    if text.chars().map(columns).sum::<usize>() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + columns(c) > width - 1 {
            break;
        }
        used += columns(c);
        out.push(c);
    }
    out.push('…');
    out
}

/// Display columns of `c` in a terminal: 2 for East Asian wide and full-width characters and most
/// emoji, else 1 (an approximation of Unicode's East Asian Width that needs no table).
fn columns(c: char) -> usize {
    let wide = matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD);
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod search_output_tests {
    use super::fit;

    #[test]
    fn descriptions_fit_the_columns_left() {
        assert_eq!(fit("short", Some(40)), "short");
        assert_eq!(fit("a fairly long description", Some(12)), "a fairly lo…");
        // Wide characters take two columns each.
        assert_eq!(fit("日本語のパッケージです", Some(11)), "日本語のパ…");
        // Too narrow to be useful: left out; piped: never cut.
        assert_eq!(fit("anything", Some(3)), "");
        assert_eq!(fit(&"x".repeat(300), None).len(), 300);
    }
}
