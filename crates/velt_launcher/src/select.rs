//! Which toolchain a command runs with, and installing it when it is missing.

use std::fmt;
use std::path::PathBuf;

use semver::Version;
use velt_toolchain::layout::velt_exe;
use velt_toolchain::pin::{find_pin, Pin};
use velt_toolchain::{release, Requirement, Root, Toolchain};

use crate::{ENV_AUTO_INSTALL, ENV_TOOLCHAIN};

/// What the launcher works with: the root, the directory it runs in, and its environment.
#[derive(Clone, Debug)]
pub struct Context {
    pub root: Root,
    pub cwd: PathBuf,
    /// `$VELT_TOOLCHAIN`.
    pub env_toolchain: Option<String>,
    /// Unless `$VELT_TOOLCHAIN_AUTO_INSTALL` is `0` (or `false`, `no`, `off`).
    pub auto_install: bool,
    /// Where releases come from ([`release::base_url`]).
    pub base: String,
}

impl Context {
    pub fn from_env(root: Root) -> Result<Context, String> {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let auto_install = !var(ENV_AUTO_INSTALL).is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        });
        Ok(Context {
            root,
            cwd,
            env_toolchain: var(ENV_TOOLCHAIN),
            auto_install,
            base: release::base_url(),
        })
    }
}

/// What selected the toolchain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Env,
    Pin(Pin),
    Default,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::Env => write!(f, "${ENV_TOOLCHAIN}"),
            Reason::Pin(pin) => write!(
                f,
                "velt: \"{}\" in {}:{}",
                pin.requirement,
                pin.manifest.display(),
                pin.line
            ),
            Reason::Default => f.write_str("the default"),
        }
    }
}

/// The toolchain asked for: a specific one, or the newest a requirement accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wanted {
    Toolchain(Toolchain),
    Requirement(Requirement),
}

impl fmt::Display for Wanted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Wanted::Toolchain(t) => write!(f, "{t}"),
            Wanted::Requirement(r) => write!(f, "{r}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub wanted: Wanted,
    pub reason: Reason,
}

/// `$VELT_TOOLCHAIN`, else the package's pin, else the default.
pub fn select(ctx: &Context) -> Result<Selection, String> {
    if let Some(text) = &ctx.env_toolchain {
        let toolchain = Toolchain::parse(text).map_err(|e| format!("${ENV_TOOLCHAIN}: {e}"))?;
        return Ok(Selection {
            wanted: Wanted::Toolchain(toolchain),
            reason: Reason::Env,
        });
    }
    if let Some(pin) = find_pin(&ctx.cwd)? {
        return Ok(Selection {
            wanted: Wanted::Requirement(pin.requirement.clone()),
            reason: Reason::Pin(pin),
        });
    }
    match ctx.root.default()? {
        Some(toolchain) => Ok(Selection {
            wanted: Wanted::Toolchain(toolchain),
            reason: Reason::Default,
        }),
        None => Err(format!(
            "no velt toolchain is selected: there is no `velt` field in a package.vlt here and no \
             default; run `velt toolchain install <version>` (the first one installed becomes \
             the default) or `velt toolchain default <version>` ({} has the toolchains)",
            ctx.root.toolchains_dir().display()
        )),
    }
}

/// The installed toolchain that satisfies `selection`, if any.
pub fn installed(root: &Root, selection: &Selection) -> Option<Toolchain> {
    match &selection.wanted {
        Wanted::Toolchain(t) => root.is_installed(t).then(|| t.clone()),
        Wanted::Requirement(req) => req
            .best(&root.versions())
            .map(|v| Toolchain::Version(v.clone())),
    }
}

/// A toolchain ready to run.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub toolchain: Toolchain,
    pub prefix: PathBuf,
    pub reason: Reason,
}

impl Resolved {
    /// `0.1.4 (velt: "0.1" in /p/package.vlt:4)`, for `$VELT_TOOLCHAIN_SELECTED` and `which`.
    pub fn describe(&self) -> String {
        format!("{} ({})", self.toolchain, self.reason)
    }

    pub fn velt(&self) -> PathBuf {
        velt_exe(&self.prefix)
    }
}

/// The toolchain `selection` runs with, installing it first when it is missing (and installing
/// is allowed).
pub fn resolve(ctx: &Context, selection: &Selection) -> Result<Resolved, String> {
    let root = &ctx.root;
    let done = |toolchain: Toolchain| -> Result<Resolved, String> {
        Ok(Resolved {
            prefix: root.prefix(&toolchain)?,
            toolchain,
            reason: selection.reason.clone(),
        })
    };
    if let Some(toolchain) = installed(root, selection) {
        return done(toolchain);
    }
    let reason = &selection.reason;
    let version = match &selection.wanted {
        Wanted::Toolchain(Toolchain::Linked(name)) => {
            let prefix = root.prefix(&Toolchain::Linked(name.clone()))?;
            return Err(format!(
                "the toolchain linked as `{name}` ({reason}) has no {}",
                velt_exe(&prefix).display()
            ));
        }
        Wanted::Toolchain(Toolchain::Version(v)) => v.clone(),
        Wanted::Requirement(req) => {
            if !ctx.auto_install {
                return Err(not_installed(&selection.wanted, reason));
            }
            match req.exact_version() {
                Some(v) => v,
                None => newest_published(ctx, req).map_err(|e| {
                    format!("no installed velt matches `{req}` ({reason}), and {e}")
                })?,
            }
        }
    };
    if !ctx.auto_install {
        return Err(not_installed(&selection.wanted, reason));
    }
    eprintln!(
        "velt: installing velt {version} ({reason}) from {}",
        ctx.base
    );
    install(ctx, &version)?;
    done(Toolchain::Version(version))
}

fn not_installed(wanted: &Wanted, reason: &Reason) -> String {
    let what = match wanted {
        Wanted::Toolchain(t) => format!("velt {t} is not installed"),
        Wanted::Requirement(r) => format!("no installed velt matches `{r}`"),
    };
    format!(
        "{what} ({reason}); run `velt toolchain install {wanted}` (installing on first use is \
         off: ${ENV_AUTO_INSTALL}=0)"
    )
}

/// The newest published version `req` accepts (not a yanked one, unless `req` names exactly it).
pub fn newest_published(ctx: &Context, req: &Requirement) -> Result<Version, String> {
    let published = release::fetch_index(&ctx.base)
        .map_err(|e| format!("the list of published versions is unavailable: {e}"))?;
    if let Some(release) = release::newest_match(&published, req) {
        return Ok(release.version.clone());
    }
    {
        let shown: Vec<String> = published
            .iter()
            .filter(|r| r.yanked.is_none())
            .map(|r| r.version.to_string())
            .collect();
        Err(format!(
            "no published velt matches `{req}`; the published versions are {}",
            if shown.is_empty() {
                "none".to_string()
            } else {
                shown.join(", ")
            }
        ))
    }
}

/// Install release `version`; another launcher finishing the same install first is fine.
pub fn install(ctx: &Context, version: &Version) -> Result<PathBuf, String> {
    match release::install_toolchain(&ctx.root, version, &ctx.base) {
        Ok(prefix) => Ok(prefix),
        Err(_) if ctx.root.is_installed(&Toolchain::Version(version.clone())) => {
            Ok(ctx.root.version_dir(version))
        }
        Err(e) => Err(e),
    }
}
