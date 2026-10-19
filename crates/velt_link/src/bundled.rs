//! The bundled linker: an `lld` shipped with the toolchain (`<prefix>/lib/velt/lld[.exe]`) plus a
//! link kit for the target ([`crate::kit`]), so `velt build` needs no system linker. Used
//! whenever both are there; otherwise the system linker links as before.
//!
//! `$VELT_LINKER` chooses: unset or empty for that default, `bundled` to require the bundled
//! linker (an error says what is missing), `system` for the system linker, any other value is
//! the path of a linker program run with the system linker's arguments.
//!
//! In a checkout, `cargo run -p velt_link --bin velt-kit -- build --target <host> --out
//! target/lib/targets/<host>` writes a kit `velt` finds, and the Rust toolchain's `rust-lld`
//! stands in for the bundled lld.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::kit::{self, Kit};

/// How [`crate::link`] links a target.
#[derive(Debug)]
pub(crate) enum Choice {
    /// The bundled lld with a kit.
    Bundled(Bundled),
    /// `$VELT_LINKER` names a program, run with the system linker's arguments.
    Override,
    /// The system linker (`link.exe`, `cc`). `why` says why the bundled one is not used, when
    /// it was not asked for explicitly.
    System { why: Option<String> },
}

/// The bundled lld and the kit for one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundled {
    pub lld: PathBuf,
    pub kit: Kit,
}

impl Bundled {
    /// `lld -flavor <flavor>`.
    pub(crate) fn command(&self, flavor: &str) -> Command {
        let mut cmd = Command::new(&self.lld);
        cmd.args(["-flavor", flavor]);
        cmd
    }
}

/// What `$VELT_LINKER` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Default,
    Bundled,
    System,
    Program(PathBuf),
}

impl Request {
    pub(crate) fn from_env() -> Request {
        Request::parse(std::env::var_os("VELT_LINKER"))
    }

    pub(crate) fn parse(value: Option<OsString>) -> Request {
        match value {
            None => Request::Default,
            Some(v) if v.is_empty() => Request::Default,
            Some(v) if v == "bundled" => Request::Bundled,
            Some(v) if v == "system" => Request::System,
            Some(v) => Request::Program(v.into()),
        }
    }
}

/// Decide how to link `target`: see the module docs. `bundled` finds the lld and kit (or says
/// why it cannot); it is only consulted when the request allows the bundled linker.
pub(crate) fn choose(
    request: Request,
    bundled: impl FnOnce() -> Result<Bundled, String>,
) -> Result<Choice, String> {
    match request {
        Request::Program(_) => Ok(Choice::Override),
        Request::System => Ok(Choice::System { why: None }),
        Request::Bundled => bundled()
            .map(Choice::Bundled)
            .map_err(|e| format!("$VELT_LINKER=bundled, but {e}")),
        Request::Default => Ok(match bundled() {
            Ok(b) => Choice::Bundled(b),
            Err(why) => Choice::System { why: Some(why) },
        }),
    }
}

/// The bundled lld and the kit for `target`, found next to the running `velt`.
pub(crate) fn find(target: &str) -> Result<Bundled, String> {
    let exe = std::env::current_exe().ok();
    let dirs = crate::native_search_dirs(exe.as_deref());
    find_in(target, &dirs, find_lld)
}

/// Testable core of [`find`]: `dirs` are the runtime search directories.
pub(crate) fn find_in(
    target: &str,
    dirs: &[PathBuf],
    lld: impl FnOnce() -> Option<PathBuf>,
) -> Result<Bundled, String> {
    if kit::KitKind::for_target(target).is_none() {
        return Err(format!("the bundled linker does not support `{target}`"));
    }
    let kit = kit::find_in(&kit::kit_dirs(dirs, target), target)?;
    let lld = lld().ok_or("the toolchain has no bundled lld (lib/velt/lld)")?;
    Ok(Bundled { lld, kit })
}

/// The bundled lld: `lib/velt/lld[.exe]` of the installed toolchain (or of `target/` in a
/// checkout), else the Rust toolchain's `rust-lld` (same program, under another name).
pub fn find_lld() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let name = format!("lld{}", std::env::consts::EXE_SUFFIX);
    crate::native_search_dirs(exe.as_deref())
        .into_iter()
        .map(|d| d.join("velt").join(&name))
        .find(|p| p.is_file())
        .or_else(crate::wasm::rust_lld)
}

/// What identifies the bundled linker a link used (for build stamps): lld and kit paths and the
/// kit stamp's modification time.
pub fn identity(b: &Bundled) -> String {
    let mtime = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| format!("{t:?}"))
            .unwrap_or_default()
    };
    format!(
        "bundled {} {} {} {}",
        b.lld.display(),
        mtime(&b.lld),
        b.kit.dir.display(),
        mtime(&b.kit.dir.join(kit::STAMP))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> Result<Bundled, String> {
        Ok(Bundled {
            lld: "lld".into(),
            kit: Kit {
                dir: "kit".into(),
                kind: kit::KitKind::MacOs,
                arch: kit::Arch::Aarch64,
            },
        })
    }

    fn missing() -> Result<Bundled, String> {
        Err("no link kit".into())
    }

    #[test]
    fn requests() {
        assert_eq!(Request::parse(None), Request::Default);
        assert_eq!(Request::parse(Some("".into())), Request::Default);
        assert_eq!(Request::parse(Some("bundled".into())), Request::Bundled);
        assert_eq!(Request::parse(Some("system".into())), Request::System);
        assert_eq!(
            Request::parse(Some("/usr/bin/ld.gold".into())),
            Request::Program("/usr/bin/ld.gold".into())
        );
    }

    #[test]
    fn precedence() {
        assert!(matches!(
            choose(Request::Default, fake).unwrap(),
            Choice::Bundled(_)
        ));
        match choose(Request::Default, missing).unwrap() {
            Choice::System { why } => assert_eq!(why.as_deref(), Some("no link kit")),
            other => panic!("{other:?}"),
        }
        // An explicit choice never looks for the bundled linker.
        let unreachable = || -> Result<Bundled, String> { panic!("looked for the bundled linker") };
        assert!(matches!(
            choose(Request::System, unreachable).unwrap(),
            Choice::System { why: None }
        ));
        assert!(matches!(
            choose(Request::Program("x".into()), unreachable).unwrap(),
            Choice::Override
        ));
        let err = choose(Request::Bundled, missing).unwrap_err();
        assert!(
            err.contains("$VELT_LINKER=bundled") && err.contains("no link kit"),
            "{err}"
        );
    }

    #[test]
    fn unsupported_targets_and_missing_pieces() {
        let err = find_in("riscv64gc-unknown-linux-gnu", &[], || None).unwrap_err();
        assert!(err.contains("does not support"), "{err}");
        let err = find_in("x86_64-unknown-linux-gnu", &["/nonexistent".into()], || {
            None
        })
        .unwrap_err();
        assert!(err.contains("no link kit"), "{err}");
    }
}
