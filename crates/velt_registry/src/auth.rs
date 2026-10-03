//! Users and their tokens. `<registry>/.auth/users.json` maps each user name to the SHA-256 of
//! the user's token; the token itself is shown once, when it is made, and never stored. A
//! request names its user with `Authorization: Bearer <token>`.
//!
//! A registry is **open** (anyone who can reach it may publish: a laptop or a trusted LAN) only
//! while `users.json` does not exist. Once it exists, every write needs a token, even if the file
//! lists no users, so a damaged or emptied file never opens the registry. Removing the last user
//! deletes the file, and only when asked to (`velt registry user remove --open`). The file is
//! replaced atomically (write a temporary file, then rename), so a reader never sees it half
//! written.
//!
//! Tokens are 32 random bytes from the operating system, so comparing their hashes (an
//! attacker can't choose a hash prefix) leaks nothing useful through timing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use velt_http::{Request, Response};

/// The users file, relative to the registry root (hidden from the package listing).
pub const USERS_FILE: &str = ".auth/users.json";

#[derive(Default, Serialize, Deserialize)]
struct Users {
    #[serde(default)]
    users: BTreeMap<String, String>,
}

/// Who sent a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The registry is open: every request may write.
    Anyone,
    /// The user whose token the request carries.
    User(String),
}

fn users_path(root: &Path) -> PathBuf {
    root.join(USERS_FILE)
}

/// The users, or `None` when the registry is open (no users file).
fn load(root: &Path) -> Result<Option<Users>, String> {
    let path = users_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("corrupt `{}`: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read `{}`: {e}", path.display())),
    }
}

fn save(root: &Path, users: &Users) -> Result<(), String> {
    let path = users_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
        // Only the server's user may read the token hashes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot restrict `{}`: {e}", dir.display()))?;
        }
    }
    vpm::json_file::write(&path, users)
}

fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

fn new_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("cannot make a random token: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// User names follow the package name rules (`[a-z][a-z0-9_-]*`).
pub fn check_user_name(name: &str) -> Result<(), String> {
    if vpm::manifest::is_valid_package_name(name) {
        Ok(())
    } else {
        Err(format!(
            "invalid user name `{name}` (use lowercase letters, digits, `-` and `_`, starting with a letter)"
        ))
    }
}

/// Whether the registry at `root` is open (has no users file).
pub fn is_open(root: &Path) -> Result<bool, String> {
    Ok(load(root)?.is_none())
}

/// Add user `name`; returns the new token (shown once). The first user closes an open registry.
pub fn add_user(root: &Path, name: &str) -> Result<String, String> {
    check_user_name(name)?;
    let mut users = load(root)?.unwrap_or_default();
    if users.users.contains_key(name) {
        return Err(format!(
            "user `{name}` already exists (`velt registry user token {name}` makes a new token)"
        ));
    }
    let token = new_token()?;
    users.users.insert(name.to_string(), hash(&token));
    save(root, &users)?;
    Ok(token)
}

/// Replace the token of user `name`; returns the new one (the old one stops working).
pub fn rotate_token(root: &Path, name: &str) -> Result<String, String> {
    let mut users = load(root)?.unwrap_or_default();
    let slot = users
        .users
        .get_mut(name)
        .ok_or_else(|| format!("no user `{name}`"))?;
    let token = new_token()?;
    *slot = hash(&token);
    save(root, &users)?;
    Ok(token)
}

/// Remove user `name` (packages it owns keep their other owners). Removing the last user opens
/// the registry, so it needs `open`.
pub fn remove_user(root: &Path, name: &str, open: bool) -> Result<(), String> {
    let mut users = load(root)?.unwrap_or_default();
    if users.users.remove(name).is_none() {
        return Err(format!("no user `{name}`"));
    }
    if !users.users.is_empty() {
        return save(root, &users);
    }
    if !open {
        return Err(format!(
            "`{name}` is the last user: without users anyone who can reach the registry may publish (pass `--open` to remove it anyway)"
        ));
    }
    let path = users_path(root);
    std::fs::remove_file(&path).map_err(|e| format!("cannot remove `{}`: {e}", path.display()))
}

/// Every user name, sorted.
pub fn user_names(root: &Path) -> Result<Vec<String>, String> {
    Ok(load(root)?.unwrap_or_default().users.into_keys().collect())
}

/// Whether `name` is a user of the registry.
pub fn is_user(root: &Path, name: &str) -> Result<bool, String> {
    Ok(load(root)?.is_some_and(|u| u.users.contains_key(name)))
}

/// The caller of `req`, or the 401 (500 for an unreadable users file) to answer.
pub fn caller(root: &Path, req: &Request) -> Result<Caller, Response> {
    let Some(users) = load(root).map_err(|e| Response::text(500, e))? else {
        return Ok(Caller::Anyone);
    };
    let token = req
        .header("authorization")
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .unwrap_or("");
    let hashed = hash(token);
    users
        .users
        .iter()
        .find(|(_, h)| !token.is_empty() && **h == hashed)
        .map(|(name, _)| Caller::User(name.clone()))
        .ok_or_else(|| {
            Response::text(
                401,
                "a valid `Authorization: Bearer <token>` is required (your registry user's token)",
            )
        })
}

#[cfg(test)]
mod tests;
