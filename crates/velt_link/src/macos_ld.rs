//! The system link on macOS without the compiler driver: `ld` run directly, with the arguments
//! `cc` would give it.
//!
//! `cc` is clang's driver: it works out the SDK, the platform version, the libraries every
//! program links (`-lSystem`, clang's builtins) and where `ld` is, and then runs `ld`. That
//! takes 20 to 35 ms of a link that takes `ld` itself 30 to 60 ms (hello against the shared
//! runtime: 48 ms through `cc`, 28 ms without). What the driver works out depends on the
//! toolchain and the SDK, not on the program, so it is asked once: `cc -###` prints the `ld`
//! command it would run for a link, and that command, with the link's own arguments cut out, is
//! kept as a [`Template`] in the user's cache directory. A later link fills the template in and
//! runs `ld`.
//!
//! The template is kept per driver, architecture, deployment target and developer directory
//! (`$SDKROOT`, `$DEVELOPER_DIR`, what `xcode-select` points at), and holds the size and time of
//! the `ld` program it names: another toolchain, an updated one or another SDK asks the driver
//! again. Anything this does not know for certain falls back to the link through `cc`, which is
//! the link there was before: an argument of the link it does not translate ([`for_ld`]), a
//! driver whose output is not clang's, no cache directory, and a direct link that fails (its
//! error would be `ld`'s, where `cc`'s is the one users and tests know).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether a link for `os` tries `ld` directly: on macOS, unless `$VELT_LINKER` names the
/// linker program (`overridden`).
pub(crate) fn applies(os: crate::TargetOs, overridden: bool) -> bool {
    os == crate::TargetOs::MacOs && !overridden
}

/// A direct link: the `ld` command, and what its template was kept as.
pub(crate) struct Direct {
    pub(crate) command: Command,
    pub(crate) kept: Kept,
}

/// The file a template is kept in.
pub(crate) struct Kept(PathBuf);

impl Kept {
    /// The link failed: what was kept is dropped, so the next link asks the driver again. A
    /// template can go stale without `ld` changing (the SDK or clang's builtins moved), and
    /// left in place it would cost every later link a failed `ld` before its `cc`.
    pub(crate) fn forget(&self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The direct link for `cc <cc_args>` (the arguments of `system::unix_args`), or `None` where
/// the link goes through `cc`.
pub(crate) fn direct(cc_args: &[OsString]) -> Option<Direct> {
    let cache = cache_dir()?;
    let driver = on_path(&std::env::var_os("PATH")?, "cc")?;
    direct_with(cc_args, &driver, &cache)
}

/// [`direct`] with the driver `driver` and templates kept in `cache`.
pub(crate) fn direct_with(cc_args: &[OsString], driver: &Path, cache: &Path) -> Option<Direct> {
    let link = for_ld(cc_args)?;
    let key = key(driver, &link);
    let file = cache.join(format!("{:016x}", hash(&key)));
    let template = match Template::read(&file, &key) {
        Some(Answer::Template(t)) => t,
        Some(Answer::None) => return None,
        None => {
            let asked = ask_driver(driver, &link);
            let kept = match &asked {
                Some(t) => t.render(&key),
                None => format!("{HEADER}\n{key}\nnone\n{END}\n"),
            };
            let _ = write_atomically(&file, &kept);
            asked?
        }
    };
    let mut cmd = Command::new(&template.program);
    cmd.args(&template.before)
        .arg("-o")
        .arg(&link.output)
        .args(&link.search)
        .args(&template.between)
        .args(&link.rest)
        .args(&template.after);
    Some(Direct {
        command: cmd,
        kept: Kept(file),
    })
}

/// A link's own arguments, as `ld` takes them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Link {
    arch: String,
    min_version: String,
    output: OsString,
    /// Its `-L` directories: before the driver's own, as the driver puts them.
    search: Vec<OsString>,
    /// Its inputs, libraries and linker options, in order.
    rest: Vec<OsString>,
}

/// The arguments `cc_args` of a link through `cc`, as `ld` takes them: `None` for one this does
/// not know (the link then goes through `cc`). Known: `-arch`, `-mmacosx-version-min=`, `-o`,
/// input files, `-l`, `-L`, `-framework` and `-Wl,` (the linker's own options).
pub(crate) fn for_ld(cc_args: &[OsString]) -> Option<Link> {
    let (mut arch, mut min_version, mut output) = (None, None, None);
    let (mut search, mut rest) = (vec![], vec![]);
    let mut args = cc_args.iter();
    while let Some(arg) = args.next() {
        let text = arg.to_str()?;
        if text == "-arch" {
            arch = Some(args.next()?.to_str()?.to_string());
        } else if let Some(v) = text.strip_prefix("-mmacosx-version-min=") {
            min_version = Some(v.to_string());
        } else if text == "-o" {
            output = Some(args.next()?.clone());
        } else if text == "-framework" {
            rest.push(arg.clone());
            rest.push(args.next()?.clone());
        } else if let Some(options) = text.strip_prefix("-Wl,") {
            rest.extend(options.split(',').map(OsString::from));
        } else if text.starts_with("-L") && text.len() > 2 {
            search.push(arg.clone());
        } else if text.starts_with("-l") && text.len() > 2 || !text.starts_with('-') {
            rest.push(arg.clone());
        } else {
            return None;
        }
    }
    Some(Link {
        arch: arch?,
        min_version: min_version?,
        output: output?,
        search,
        rest,
    })
}

/// The `ld` command the driver runs for a link, without the link's own arguments.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Template {
    program: PathBuf,
    /// Before `-o <output>`.
    before: Vec<String>,
    /// Between the output and the inputs (the driver's own `-L` directories).
    between: Vec<String>,
    /// After the inputs (`-lSystem`, clang's builtins).
    after: Vec<String>,
}

/// What a cache file holds: a template, or that the driver gave none.
enum Answer {
    Template(Template),
    None,
}

const HEADER: &str = "velt macos ld 2";
/// A cache file's last line: one without it was not written whole, and is not read.
const END: &str = "end";
/// The stand-ins for a link's output and input in the command asked of the driver.
const OUTPUT: &str = "velt-ld-template-output";

/// Ask `driver` for the `ld` command of a link like `link` (`cc -###`, which runs nothing).
fn ask_driver(driver: &Path, link: &Link) -> Option<Template> {
    // An input the driver finds (it checks that inputs exist): the driver itself.
    let input = driver.to_str()?;
    let out = Command::new(driver)
        .args(["-###", "-arch", &link.arch])
        .arg(format!("-mmacosx-version-min={}", link.min_version))
        .args([input, "-o", OUTPUT])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stderr).ok()?;
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('"'))?;
    Template::of(&quoted(line)?, input)
}

/// The arguments of a command line as `cc -###` prints it: each in double quotes, `\` before a
/// quote or a backslash inside.
pub(crate) fn quoted(line: &str) -> Option<Vec<String>> {
    let mut args = vec![];
    let mut chars = line.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {}
            '"' => {
                let mut arg = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => arg.push(chars.next()?),
                        c => arg.push(c),
                    }
                }
                args.push(arg);
            }
            _ => return None,
        }
    }
    Some(args)
}

impl Template {
    /// The template of the driver's command `command` (program first), asked with the input
    /// `input` and the output [`OUTPUT`]: `None` unless it is `ld`'s, with the output before the
    /// input and both there once.
    pub(crate) fn of(command: &[String], input: &str) -> Option<Template> {
        let (program, args) = command.split_first()?;
        let program = PathBuf::from(program);
        if !program.is_absolute() || program.file_name() != Some(OsStr::new("ld")) {
            return None;
        }
        let o = args.iter().position(|a| a == OUTPUT)?;
        let i = args.iter().position(|a| a == input)?;
        let once = |name: &str| args.iter().filter(|a| *a == name).count() == 1;
        if o == 0 || args[o - 1] != "-o" || i < o || !once(OUTPUT) || !once(input) {
            return None;
        }
        let plain = |a: &String| !a.contains('\n');
        if !args.iter().all(plain) {
            return None;
        }
        Some(Template {
            program,
            before: args[..o - 1].to_vec(),
            between: args[o + 1..i].to_vec(),
            after: args[i + 1..].to_vec(),
        })
    }

    /// The cache file's text for this template under `key`: the program with its size and
    /// time, then an argument per line.
    fn render(&self, key: &str) -> String {
        let mut s = format!("{HEADER}\n{key}\n");
        s.push_str(&format!(
            "ld {}\n",
            stamp(&self.program).unwrap_or_default()
        ));
        s.push_str(&format!("program {}\n", self.program.display()));
        for (tag, args) in [
            ("b", &self.before),
            ("m", &self.between),
            ("a", &self.after),
        ] {
            for a in args {
                s.push_str(&format!("{tag} {a}\n"));
            }
        }
        s.push_str(END);
        s.push('\n');
        s
    }

    /// What `file` keeps for `key`: `None` when it is not there, is for another key or names a
    /// program that is gone or changed.
    fn read(file: &Path, key: &str) -> Option<Answer> {
        let text = std::fs::read_to_string(file).ok()?;
        // Written whole: the last line is the end marker, a line of its own.
        let text = text.strip_suffix(&format!("\n{END}\n"))?;
        let mut lines = text.lines();
        if lines.next()? != HEADER || lines.next()? != key {
            return None;
        }
        let first = lines.next()?;
        if first == "none" {
            return Some(Answer::None);
        }
        let stamped = first.strip_prefix("ld ")?;
        let program = PathBuf::from(lines.next()?.strip_prefix("program ")?);
        if stamp(&program)? != stamped {
            return None;
        }
        let (mut before, mut between, mut after) = (vec![], vec![], vec![]);
        for line in lines {
            let (tag, arg) = line.split_once(' ')?;
            match tag {
                "b" => before.push(arg.to_string()),
                "m" => between.push(arg.to_string()),
                "a" => after.push(arg.to_string()),
                _ => return None,
            }
        }
        Some(Answer::Template(Template {
            program,
            before,
            between,
            after,
        }))
    }
}

/// The size and modification time of `program`, which change when a toolchain is updated.
fn stamp(program: &Path) -> Option<String> {
    let m = std::fs::metadata(program).ok()?;
    let time = m.modified().ok()?;
    let since = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{} {}", m.len(), since.as_nanos()))
}

/// What a template is kept for: the driver (with its size and time), the link's architecture
/// and deployment target, and what selects the toolchain and the SDK.
fn key(driver: &Path, link: &Link) -> String {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let selected = std::fs::read_link("/var/db/xcode_select_link")
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!(
        "{} {} | {} | {} | SDKROOT={} | DEVELOPER_DIR={} | {selected}",
        driver.display(),
        stamp(driver).unwrap_or_default(),
        link.arch,
        link.min_version,
        env("SDKROOT"),
        env("DEVELOPER_DIR"),
    )
    .replace('\n', " ")
}

/// FNV-1a of `text`: a cache file's name.
fn hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Where the templates are kept: the user's own cache directory (`~/Library/Caches/velt/ld`),
/// which no other user writes; `None` without a home directory.
fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Library/Caches/velt/ld"))
}

/// The first program named `name` on the search path `path`, where that is in a directory
/// given in full. `cc` in the directory the build runs in (an empty or relative entry, `.`) is
/// whatever a project put there: the link then goes through `cc` as the system finds it, and
/// nothing is asked of that program or kept for it.
fn on_path(path: &OsStr, name: &str) -> Option<PathBuf> {
    let first = std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())?;
    first.is_absolute().then_some(first)
}

/// Write `text` to `file` whole: under a name of this write's own first (the process and a
/// count, made anew: two links at once, in one process too, never write one file), then renamed.
fn write_atomically(file: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = file.as_os_str().to_owned();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    temp.push(format!(".{}-{n}", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = (std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp))
    .and_then(|mut f| f.write_all(text.as_bytes()))
    .and_then(|()| std::fs::rename(&temp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
#[path = "macos_ld_tests.rs"]
mod tests;
