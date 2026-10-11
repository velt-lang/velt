//! `velt toolchain …`: the launcher's own command, the same whichever version a package pins.

use std::ffi::OsString;
use std::path::Path;

use semver::Version;
use velt_toolchain::{Requirement, Toolchain};

use crate::select::{self, Context, Wanted};

pub const HELP: &str = "\
Manage the installed velt toolchains

Usage: velt toolchain <command>

Commands:
  list [--available]           installed toolchains (* the default, > the one selected here);
                               --available lists the published versions (and the yanked)
  install <version> [--default]
                               install a release: `0.1.3`, or the newest match of `0.1`; the
                               first one installed becomes the default
  install --from <file> [--unverified] [--default]
                               install a downloaded velt-<version>-<triple>.tar.gz, checked
                               against the SHA256SUMS and SHA256SUMS.sig beside it
  remove <toolchain> [--force] remove a version (debug executables it built stop running) or a
                               link; --force removes the default too
  self-update                  replace this launcher with the newest release's
  prune [--dry-run]            remove every version but the default and the newest of each
                               major.minor; --dry-run lists them
  default [<toolchain>]        show or set the toolchain used outside pinned packages
  which                        the toolchain this directory selects, and why
  link <name> <prefix>         use a toolchain built elsewhere (a checkout) as <name>
  unlink <name>                remove a link (the prefix stays)
  --version                    the launcher's own version (`velt --version` is the toolchain's)

A package selects versions with `velt: \"0.1\"` in package.vlt; `velt +<toolchain> <command>` or
$VELT_TOOLCHAIN (a version or a link) overrides it for one command. Missing versions are installed on first use unless
$VELT_TOOLCHAIN_AUTO_INSTALL=0; $VELT_INSTALL_BASE_URL names a mirror.
";

/// Run `velt toolchain <args>`.
pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let args: Vec<&str> = args
        .iter()
        .map(|a| {
            a.to_str()
                .ok_or_else(|| format!("argument {a:?} is not valid UTF-8"))
        })
        .collect::<Result<_, _>>()?;
    let (command, rest) = match args.split_first() {
        Some((c, rest)) => (*c, rest),
        None => {
            print!("{HELP}");
            return Ok(());
        }
    };
    if matches!(command, "--version" | "-V") {
        // The installers compare it with the launcher they would install.
        println!("velt-launcher {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let flag = |name: &str| rest.contains(&name);
    let positional: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let known_flags: &[&str] = match command {
        "list" => &["--available"],
        "install" => &["--default", "--from", "--unverified"],
        "remove" => &["--force"],
        "prune" => &["--dry-run"],
        "self-update" => &[],
        _ => &[],
    };
    if let Some(bad) = rest
        .iter()
        .find(|a| a.starts_with('-') && !known_flags.contains(a))
    {
        if matches!(*bad, "-h" | "--help") {
            print!("{HELP}");
            return Ok(());
        }
        return Err(format!(
            "unknown option `{bad}` for `velt toolchain {command}`"
        ));
    }
    let arity = |n: usize, usage: &str| {
        if positional.len() == n {
            Ok(())
        } else {
            Err(format!("usage: velt toolchain {usage}"))
        }
    };
    match command {
        "list" => {
            arity(0, "list [--available]")?;
            list(ctx, flag("--available"))
        }
        "install" => {
            // `--from <file>` takes a value, which the generic parsing counts as positional.
            if let Some(at) = rest.iter().position(|a| *a == "--from") {
                let file = rest.get(at + 1).filter(|f| !f.starts_with('-')).ok_or(
                    "usage: velt toolchain install --from <file> [--unverified] [--default]",
                )?;
                if positional.len() != 1 {
                    return Err(
                        "usage: velt toolchain install --from <file> [--unverified] [--default]"
                            .into(),
                    );
                }
                return install_from(
                    ctx,
                    Path::new(file),
                    flag("--unverified"),
                    flag("--default"),
                );
            }
            if flag("--unverified") {
                return Err("--unverified goes with --from <file>".into());
            }
            arity(1, "install <version> [--default]")?;
            install(ctx, positional[0], flag("--default"))
        }
        "remove" => {
            arity(1, "remove <toolchain> [--force]")?;
            remove(ctx, positional[0], flag("--force"))
        }
        "self-update" => {
            arity(0, "self-update")?;
            self_update(ctx)
        }
        "prune" => {
            arity(0, "prune [--dry-run]")?;
            prune(ctx, flag("--dry-run"))
        }
        "default" => match positional.as_slice() {
            [] => show_default(ctx),
            [t] => set_default(ctx, t),
            _ => Err("usage: velt toolchain default [<toolchain>]".into()),
        },
        "which" => {
            arity(0, "which")?;
            which(ctx)
        }
        "link" => {
            arity(2, "link <name> <prefix>")?;
            link(ctx, positional[0], Path::new(positional[1]))
        }
        "unlink" => {
            arity(1, "unlink <name>")?;
            let name = positional[0];
            velt_toolchain::layout::check_link_name(name)?;
            ctx.root.remove(&Toolchain::Linked(name.into()))?;
            println!("unlinked {name}");
            Ok(())
        }
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(format!(
            "unknown command `velt toolchain {other}` (see `velt toolchain help`)"
        )),
    }
}

/// The toolchain the current directory selects, when installed (for `list`'s marker).
fn selected_here(ctx: &Context) -> Option<Toolchain> {
    let selection = select::select(ctx).ok()?;
    select::installed(&ctx.root, &selection)
}

fn list(ctx: &Context, available: bool) -> Result<(), String> {
    let root = &ctx.root;
    let installed = root.versions();
    if available {
        for release in select::index(ctx)?.releases.iter().rev() {
            let v = &release.version;
            let mut state = String::new();
            if installed.contains(v) {
                state.push_str("  (installed)");
            }
            if let Some(why) = &release.yanked {
                state.push_str(&format!("  (yanked: {why})"));
            }
            println!("{v}{state}");
        }
        return Ok(());
    }
    let default = root.default()?;
    let here = selected_here(ctx);
    let marks = |t: &Toolchain| {
        let mut s = String::new();
        s.push(if default.as_ref() == Some(t) {
            '*'
        } else {
            ' '
        });
        s.push(if here.as_ref() == Some(t) { '>' } else { ' ' });
        s
    };
    if installed.is_empty() && root.links().is_empty() {
        println!(
            "no toolchains installed in {} (`velt toolchain install <version>`)",
            root.toolchains_dir().display()
        );
        return Ok(());
    }
    for v in installed.iter().rev() {
        let t = Toolchain::Version(v.clone());
        println!("{} {v}", marks(&t));
    }
    for (name, prefix) in root.links() {
        let t = Toolchain::Linked(name.clone());
        let broken = if root.is_installed(&t) {
            ""
        } else {
            "  (missing bin/velt)"
        };
        println!("{} {name} -> {}{broken}", marks(&t), prefix.display());
    }
    Ok(())
}

/// `0.1.3` is that version; anything else is a requirement whose newest published match is
/// installed.
fn install(ctx: &Context, spec: &str, make_default: bool) -> Result<(), String> {
    let version = match Version::parse(spec.trim()) {
        Ok(v) => v,
        Err(_) => {
            let req = Requirement::parse(spec)?;
            match req.exact_version() {
                Some(v) => v,
                None => select::newest_published(ctx, &req)?,
            }
        }
    };
    let toolchain = Toolchain::Version(version.clone());
    let root = &ctx.root;
    if root.is_installed(&toolchain) {
        println!("velt {version} is already installed");
    } else {
        eprintln!("installing velt {version} from {}...", ctx.base);
        let prefix = select::install(ctx, &version)?;
        println!("installed velt {version} into {}", prefix.display());
    }
    if make_default || root.default()?.is_none() {
        root.set_default(&toolchain)?;
        println!("velt {version} is the default");
    }
    Ok(())
}

/// `install --from <file>`: a toolchain archive downloaded before (no network needed).
fn install_from(
    ctx: &Context,
    file: &Path,
    unverified: bool,
    make_default: bool,
) -> Result<(), String> {
    let (version, installed) = select::install_from(ctx, file, unverified)?;
    if installed {
        println!(
            "installed velt {version} from {} into {}",
            file.display(),
            ctx.root.version_dir(&version).display()
        );
    } else {
        println!(
            "velt {version} is already installed; kept it (did not install {})",
            file.display()
        );
    }
    let toolchain = Toolchain::Version(version.clone());
    if make_default || ctx.root.default()?.is_none() {
        ctx.root.set_default(&toolchain)?;
        println!("velt {version} is the default");
    }
    Ok(())
}

fn remove(ctx: &Context, text: &str, force: bool) -> Result<(), String> {
    let toolchain = Toolchain::parse(text)?;
    let root = &ctx.root;
    if root.default()?.as_ref() == Some(&toolchain) && !force {
        return Err(format!(
            "{toolchain} is the default; choose another (`velt toolchain default <toolchain>`) \
             or pass --force"
        ));
    }
    root.remove(&toolchain)?;
    match toolchain {
        Toolchain::Version(v) => {
            println!("removed velt {v}; debug executables it built no longer run (rebuild them)")
        }
        Toolchain::Linked(name) => println!("unlinked {name}"),
    }
    Ok(())
}

/// Replace this launcher with the newest one the signed index names: the `bin/velt-launcher` of
/// that velt's toolchain, installed first when it isn't (checked as every install is).
fn self_update(ctx: &Context) -> Result<(), String> {
    let dest = launcher_path(&ctx.root);
    // Only the launcher at `<root>/bin/velt` replaces it: one copied elsewhere in `bin` would
    // overwrite a `velt` it isn't.
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot find the launcher's own path: {e}"))?;
    if std::fs::canonicalize(&dest).ok().as_ref() != Some(&exe) {
        return Err(format!(
            "this launcher ({}) is not {}, so it can't update it; run that one",
            exe.display(),
            dest.display()
        ));
    }
    let own = Version::parse(env!("CARGO_PKG_VERSION")).expect("ICE: the crate version");
    let index = velt_toolchain::release::fetch_index(&ctx.root, &ctx.base)?;
    let Some(newest) = index.newer_launcher(&own).cloned() else {
        println!("the velt launcher is up to date ({own})");
        return Ok(());
    };
    let toolchain = Toolchain::Version(newest.clone());
    let prefix = if ctx.root.is_installed(&toolchain) {
        ctx.root.version_dir(&newest)
    } else {
        eprintln!("installing velt {newest}, which brings the launcher...");
        select::install(ctx, &newest)?
    };
    let launcher = prefix
        .join("bin")
        .join(format!("velt-launcher{}", std::env::consts::EXE_SUFFIX));
    if !launcher.is_file() {
        return Err(format!(
            "velt {newest} has no launcher ({}); run the installer again",
            launcher.display()
        ));
    }
    replace_launcher(&ctx.root, &launcher, &newest)?;
    println!("updated the velt launcher: {own} → {newest}");
    Ok(())
}

/// `<root>/bin/velt`.
fn launcher_path(root: &velt_toolchain::Root) -> std::path::PathBuf {
    root.dir()
        .join("bin")
        .join(format!("velt{}", std::env::consts::EXE_SUFFIX))
}

/// Put `new` (the launcher of velt `version`) in place of `<root>/bin/velt`: copied beside it
/// (`.velt-new-<pid>`), run there to check it is a working launcher of that version, then renamed
/// over it, so `velt` is never half written nor replaced by a program that can't run here (another
/// libc or architecture). A running `velt.exe` can't be replaced on Windows, but can be renamed:
/// it is moved aside first (`.velt.exe.old-*`, deleted by a later run, `crate::sweep`).
fn replace_launcher(
    root: &velt_toolchain::Root,
    new: &Path,
    version: &Version,
) -> Result<(), String> {
    let bin = root.dir().join("bin");
    let exe = std::env::consts::EXE_SUFFIX;
    let dest = launcher_path(root);
    let staged = bin.join(format!(".velt-new-{}{exe}", std::process::id()));
    let result = stage_launcher(new, &staged, version).and_then(|()| {
        let aside = bin.join(format!(".velt{exe}.old-{}", std::process::id()));
        let moved_aside = cfg!(windows) && dest.exists() && std::fs::rename(&dest, &aside).is_ok();
        std::fs::rename(&staged, &dest).map_err(|e| {
            if moved_aside {
                let _ = std::fs::rename(&aside, &dest);
            }
            format!("cannot replace {}: {e}", dest.display())
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Copy `new` to `staged`, executable, and check that it runs and is velt-launcher `version`.
fn stage_launcher(new: &Path, staged: &Path, version: &Version) -> Result<(), String> {
    std::fs::copy(new, staged).map_err(|e| format!("cannot copy {}: {e}", new.display()))?;
    // Fresh, whatever the copy kept: `crate::sweep` deletes only stale staged launchers, and this
    // one runs (and sweeps) before it is renamed.
    std::fs::File::options()
        .write(true)
        .open(staged)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()))
        .map_err(|e| format!("cannot write {}: {e}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", staged.display()))?;
    }
    let refuse = |why: String| {
        format!(
            "the launcher of velt {version} ({}) {why}; kept the current one",
            new.display()
        )
    };
    let out = std::process::Command::new(staged)
        .args(["toolchain", "--version"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| refuse(format!("does not run here: {e}")))?;
    let said = String::from_utf8_lossy(&out.stdout);
    let expected = format!("velt-launcher {version}");
    if !out.status.success() || said.trim() != expected {
        return Err(refuse(format!(
            "does not work here: `toolchain --version` {} `{}`, not `{expected}`",
            if out.status.success() {
                "printed".to_string()
            } else {
                format!("failed ({}) and printed", out.status)
            },
            said.trim()
        )));
    }
    Ok(())
}

/// Remove the versions [`velt_toolchain::layout::prunable`] names, saying how much each frees.
/// One in use (Windows) is reported and kept, and the others are still removed.
fn prune(ctx: &Context, dry_run: bool) -> Result<(), String> {
    let root = &ctx.root;
    // Kept besides the newest of each minor: the default, the version the current directory
    // selects (a package's pin or lock), and versions a link points into.
    let mut keep = root.linked_versions();
    let chosen = [root.default()?, selected_here(ctx)];
    keep.extend(chosen.into_iter().flatten().filter_map(|t| match t {
        Toolchain::Version(v) => Some(v),
        Toolchain::Linked(_) => None,
    }));
    let prunable = velt_toolchain::layout::prunable(&root.versions(), &keep);
    if prunable.is_empty() {
        println!(
            "nothing to prune: each installed version is the default, the newest of its minor, \
             the one this directory selects, or one a link points into"
        );
        return Ok(());
    }
    let mut freed = 0;
    let mut failed = vec![];
    for v in &prunable {
        let size = velt_toolchain::layout::dir_size(&root.version_dir(v));
        if dry_run {
            println!("would remove velt {v} ({})", megabytes(size));
            freed += size;
            continue;
        }
        match root.remove(&Toolchain::Version(v.clone())) {
            Ok(()) => {
                println!("removed velt {v} ({})", megabytes(size));
                freed += size;
            }
            Err(e) => failed.push(e),
        }
    }
    if dry_run {
        println!(
            "would free {}; nothing was removed (debug executables those versions built would \
             stop running)",
            megabytes(freed)
        );
    } else {
        println!(
            "freed {}; debug executables those versions built no longer run (rebuild them)",
            megabytes(freed)
        );
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n"))
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", (bytes + 500_000) / 1_000_000)
}

fn show_default(ctx: &Context) -> Result<(), String> {
    match ctx.root.default()? {
        Some(t) => println!("{t}"),
        None => println!("no default (`velt toolchain default <toolchain>`)"),
    }
    Ok(())
}

fn set_default(ctx: &Context, text: &str) -> Result<(), String> {
    let toolchain = Toolchain::parse(text)?;
    if !ctx.root.is_installed(&toolchain) {
        return Err(format!(
            "{toolchain} is not installed (`velt toolchain install {toolchain}`)"
        ));
    }
    ctx.root.set_default(&toolchain)?;
    println!("velt {toolchain} is the default");
    Ok(())
}

fn which(ctx: &Context) -> Result<(), String> {
    let selection = select::select(ctx)?;
    match select::installed(&ctx.root, &selection) {
        Some(toolchain) => {
            let prefix = ctx.root.prefix(&toolchain)?;
            println!("{toolchain} ({})", selection.reason);
            println!("{}", prefix.display());
        }
        None => {
            let next = match &selection.wanted {
                Wanted::Toolchain(Toolchain::Linked(_)) => "its link is broken".to_string(),
                _ if ctx.auto_install => "the next velt command installs it".to_string(),
                wanted => format!("`velt toolchain install {wanted}` installs it"),
            };
            println!(
                "{} ({}): not installed; {next}",
                selection.wanted, selection.reason
            );
        }
    }
    Ok(())
}

fn link(ctx: &Context, name: &str, prefix: &Path) -> Result<(), String> {
    ctx.root.link(name, prefix)?;
    println!(
        "linked {name} to {}; select it with VELT_TOOLCHAIN={name} or `velt toolchain default {name}`",
        prefix.display()
    );
    Ok(())
}
