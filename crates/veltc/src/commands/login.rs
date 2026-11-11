//! `velt login <url> [--device | --token-stdin]`: sign in to a registry and store its token.
//!
//! In a terminal, `velt login` asks the registry for browser sign-in (`GET /api/v1/login`,
//! [`vpm::login`]) and opens the browser at its page; `--device` signs in with a code entered on
//! another machine. A registry without browser sign-in (`velt registry serve`), `--token-stdin`,
//! or stdin that is not a terminal reads the token from stdin as before.

use std::io::{BufRead, IsTerminal};

use crate::cli::registry::LoginHow as How;
use crate::style;

/// Signs in to the registry at `url` and stores its token.
pub fn login_command(url: &str, how: How) -> Result<(), String> {
    let key = vpm::credentials::registry_key(url)?;
    vpm::credentials::check_transport(&key, "a registry token")?;
    let interactive = std::io::stdin().is_terminal();
    if how == How::TokenStdin || how == How::Auto && !interactive {
        return paste(&key);
    }
    let found = match vpm::login::discover(&key) {
        Ok(Some(found)) => found,
        Ok(None) => {
            if how == How::Device {
                style::warning(&format!(
                    "{key} has no browser sign-in: paste a token from its administrator instead"
                ));
            }
            return paste(&key);
        }
        Err(e) => {
            style::warning(&format!(
                "cannot ask {key} for browser sign-in ({e}): paste a token instead"
            ));
            return paste(&key);
        }
    };
    let mut ui = Terminal;
    let signed = match how {
        How::Device if !found.device => {
            return Err(format!(
                "{key} offers no device sign-in: run `velt login {key}` to sign in with the browser on this machine"
            ))
        }
        How::Device => vpm::login::device::sign_in(&key, &mut ui)?,
        _ => vpm::login::browser(&key, &found, &mut ui, vpm::login::CALLBACK_TIMEOUT)?,
    };
    let path = vpm::credentials::default_path()?;
    vpm::credentials::login(&path, &key, &signed.token)?;
    style::status(
        "Logged in",
        &format!(
            "to {key} as {} (token valid until {})",
            signed.user, signed.expires
        ),
    );
    Ok(())
}

/// Reads the token from stdin (one line; a prompt without echo when it is a terminal) and stores
/// it for the registry `key`.
fn paste(key: &str) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut token = String::new();
    let read = if stdin.is_terminal() {
        eprint!("Token for {key}: ");
        // The token is not shown while it is typed (or pasted); the guard restores the
        // terminal on every path.
        let _quiet = super::registry::NoEcho::start();
        let read = stdin.lock().read_line(&mut token);
        eprintln!();
        read
    } else {
        stdin.lock().read_line(&mut token)
    };
    read.map_err(|e| format!("cannot read the token from stdin: {e}"))?;
    let path = vpm::credentials::default_path()?;
    vpm::credentials::login(&path, key, &token)?;
    style::status(
        "Saved",
        &format!("the token for {key} in {}", path.display()),
    );
    Ok(())
}

/// The real terminal and browser.
struct Terminal;

impl vpm::login::Ui for Terminal {
    fn open(&mut self, url: &str) -> bool {
        open_browser(url)
    }

    fn say(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

/// Starts the system's browser at `url` (checked by [`vpm::login::is_openable`]): `open` on
/// macOS, `xdg-open` elsewhere on Unix, `cmd /c start` on Windows. Whether it started.
fn open_browser(url: &str) -> bool {
    if !vpm::login::is_openable(url) {
        return false;
    }
    match browser_command(url) {
        Some(mut cmd) => cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        None => false,
    }
}

#[cfg(target_os = "macos")]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut cmd = std::process::Command::new("open");
    cmd.arg(url);
    Some(cmd)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn browser_command(url: &str) -> Option<std::process::Command> {
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url);
    Some(cmd)
}

#[cfg(windows)]
fn browser_command(url: &str) -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt;
    // cmd.exe reads `&` as a command separator outside quotes and expands `%…%` inside them:
    // the URL goes in quotes, and one with `%` is left for the user to open.
    if url.contains('%') {
        return None;
    }
    let mut cmd = std::process::Command::new("cmd");
    cmd.raw_arg(format!("/c start \"\" \"{url}\""));
    Some(cmd)
}

#[cfg(not(any(unix, windows)))]
fn browser_command(_url: &str) -> Option<std::process::Command> {
    None
}
