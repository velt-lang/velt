//! `velt toolchain …`: the launcher's own command, the same whichever version a package pins.

use std::ffi::OsString;
use std::path::Path;

use semver::Version;
use velt_toolchain::{release, Requirement, Toolchain};

use crate::select::{self, Context, Wanted};

pub const HELP: &str = "\
Manage the installed velt toolchains

Usage: velt toolchain <command>

Commands:
  list [--available]           installed toolchains (* the default, > the one selected here);
                               --available lists the published versions
  install <version> [--default]
                               install a release: `0.1.3`, or the newest match of `0.1`; the
                               first one installed becomes the default
  remove <toolchain> [--force] remove a version (debug executables it built stop running) or a
                               link; --force removes the default too
  default [<toolchain>]        show or set the toolchain used outside pinned packages
  which                        the toolchain this directory selects, and why
  link <name> <prefix>         use a toolchain built elsewhere (a checkout) as <name>
  unlink <name>                remove a link (the prefix stays)

A package selects versions with `velt: \"0.1\"` in package.vlt; $VELT_TOOLCHAIN (a version or a
link) overrides it for one command. Missing versions are installed on first use unless
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
    let flag = |name: &str| rest.contains(&name);
    let positional: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let known_flags: &[&str] = match command {
        "list" => &["--available"],
        "install" => &["--default"],
        "remove" => &["--force"],
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
            arity(1, "install <version> [--default]")?;
            install(ctx, positional[0], flag("--default"))
        }
        "remove" => {
            arity(1, "remove <toolchain> [--force]")?;
            remove(ctx, positional[0], flag("--force"))
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
        for v in release::fetch_index(&ctx.base)?.iter().rev() {
            let state = if installed.contains(v) {
                "  (installed)"
            } else {
                ""
            };
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
