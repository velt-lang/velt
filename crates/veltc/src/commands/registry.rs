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
            velt_registry::auth::remove_user(&root, name, open)?;
            style::status("Removed", &format!("user `{name}`"));
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
            "the token of `{name}` (shown once; the user sets it as ${})",
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

/// `velt search <text>`: `name version` lines on stdout.
pub fn search_command(query: &str) -> Result<(), String> {
    let loc = locations()?;
    let hits = vpm::search::search(&loc, query)?;
    if hits.is_empty() {
        style::status(
            "Searched",
            &format!("{}: no package matches `{query}`", loc.describe()),
        );
    }
    let width = hits.iter().map(|h| h.name.len()).max().unwrap_or(0);
    for hit in hits {
        println!("{:width$}  {}", hit.name, hit.version);
    }
    Ok(())
}
