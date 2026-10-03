//! Package owners: `<registry>/<name>/owners.json` lists the users who may publish versions of a
//! package, add native libraries, yank and change its owners. The first user to publish a new
//! package becomes its owner. On a registry with users, an existing package without owners (one
//! published while the registry was open, or whose owners an administrator removed) can't be
//! changed by anyone until an administrator assigns one ([`set_by_admin`], `velt registry owner
//! add`). On an open registry owners are not enforced.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use velt_http::{Request, Response};

use crate::auth::{self, Caller};

/// The owners file inside a package's registry directory.
pub const OWNERS_FILE: &str = "owners.json";

#[derive(Default, Serialize, Deserialize)]
struct Owners {
    #[serde(default)]
    owners: Vec<String>,
}

fn path(root: &Path, name: &str) -> PathBuf {
    root.join(name).join(OWNERS_FILE)
}

/// Whether `name` is a published package of the registry (only valid names are looked up, so
/// no request can name a path outside it).
fn published(root: &Path, name: &str) -> bool {
    vpm::manifest::is_valid_package_name(name)
        && root.join(name).join(vpm::registry::INDEX_FILE).is_file()
}

/// The owners of `name` (empty when it has none).
pub fn read(root: &Path, name: &str) -> Result<Vec<String>, String> {
    let path = path(root, name);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Owners>(&text)
            .map(|o| o.owners)
            .map_err(|e| format!("corrupt `{}`: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read `{}`: {e}", path.display())),
    }
}

fn write(root: &Path, name: &str, owners: Vec<String>) -> Result<(), String> {
    vpm::json_file::write(&path(root, name), &Owners { owners })
}

/// `Ok` when `caller` may change package `name`, else the 403 (500) to answer.
pub fn authorize(root: &Path, name: &str, caller: &Caller) -> Result<(), Response> {
    let Caller::User(user) = caller else {
        return Ok(());
    };
    let owners = read(root, name).map_err(|e| Response::text(500, e))?;
    if owners.contains(user) || (owners.is_empty() && !published(root, name)) {
        return Ok(());
    }
    let why = if owners.is_empty() {
        format!(
            "`{name}` has no owners; an administrator assigns one with `velt registry owner add`"
        )
    } else {
        format!(
            "`{user}` is not an owner of `{name}` (owners: {})",
            owners.join(", ")
        )
    };
    Err(Response::text(403, why))
}

/// After `caller` published `name`: the first user to publish a new package owns it.
pub fn claim(root: &Path, name: &str, caller: &Caller) -> Result<(), String> {
    match caller {
        Caller::User(user) if read(root, name)?.is_empty() => write(root, name, vec![user.clone()]),
        _ => Ok(()),
    }
}

/// `GET /api/v1/<name>/owners`: one owner per line.
pub fn list(root: &Path, name: &str) -> Response {
    if !vpm::manifest::is_valid_package_name(name) {
        return Response::text(400, "invalid package name");
    }
    if !published(root, name) {
        return Response::text(404, format!("no package `{name}`"));
    }
    match read(root, name) {
        Ok(owners) => Response::text(
            200,
            owners.iter().map(|o| format!("{o}\n")).collect::<String>(),
        ),
        Err(e) => Response::text(500, e),
    }
}

/// `PUT` (`add`) or `DELETE /api/v1/<name>/owners/<user>`.
pub fn change(root: &Path, req: &Request, name: &str, user: &str, add: bool) -> Response {
    if !vpm::manifest::is_valid_package_name(name) {
        return Response::text(400, "invalid package name");
    }
    if let Err(e) = auth::check_user_name(user) {
        return Response::text(400, e);
    }
    let result = auth::caller(root, req).and_then(|caller| {
        if !published(root, name) {
            return Err(Response::text(404, format!("no package `{name}`")));
        }
        if caller == Caller::Anyone {
            return Err(Response::text(
                400,
                "this registry has no users, so packages have no owners",
            ));
        }
        authorize(root, name, &caller)?;
        update(root, name, user, add, false).map_err(|(status, e)| Response::text(status, e))
    });
    match result {
        Ok(owners) => Response::text(200, format!("owners of `{name}`: {}\n", owners.join(", "))),
        Err(resp) => resp,
    }
}

/// `velt registry owner add|remove`: change the owners of `name` directly in the registry
/// directory, without a token. An administrator may remove the last owner.
pub fn set_by_admin(root: &Path, name: &str, user: &str, add: bool) -> Result<Vec<String>, String> {
    if !vpm::manifest::is_valid_package_name(name) {
        return Err(format!("invalid package name `{name}`"));
    }
    auth::check_user_name(user)?;
    if !published(root, name) {
        return Err(format!("no package `{name}` in `{}`", root.display()));
    }
    update(root, name, user, add, true).map_err(|(_, e)| e)
}

/// Add or remove `user` and write the owners; `(status, message)` on failure.
fn update(
    root: &Path,
    name: &str,
    user: &str,
    add: bool,
    admin: bool,
) -> Result<Vec<String>, (u16, String)> {
    let server_error = |e: String| (500, e);
    let mut owners = read(root, name).map_err(server_error)?;
    if add {
        if !auth::is_user(root, user).map_err(server_error)? {
            return Err((404, format!("no user `{user}`")));
        }
        if !owners.iter().any(|o| o == user) {
            owners.push(user.to_string());
        }
    } else {
        if !owners.iter().any(|o| o == user) {
            return Err((404, format!("`{user}` is not an owner of `{name}`")));
        }
        owners.retain(|o| o != user);
        if owners.is_empty() && !admin {
            return Err((
                409,
                format!("`{user}` is the last owner of `{name}`; add another owner first"),
            ));
        }
    }
    write(root, name, owners.clone()).map_err(server_error)?;
    Ok(owners)
}
