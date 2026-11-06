//! Which paths and link targets an archive may hold. A path has to mean the same file on every
//! OS, so what Windows can't hold or would read differently is refused everywhere: `:` (a drive or
//! an alternate stream), `CON` and the other device names, a trailing dot or space (Windows drops
//! them), `\` (a separator there). Paths that differ only by case or Unicode normalisation are one
//! file on macOS and Windows; [`clash_key`] finds them.

use icu_normalizer::ComposingNormalizerBorrowed;

/// The longest path or link target accepted.
const MAX_PATH: usize = 4096;
/// The most segments a path may have.
const MAX_DEPTH: usize = 128;

/// `raw` as a `/`-separated relative path: `None` for the archive's root (`./`), `Err` with the
/// reason it is refused. `.` segments and a trailing `/` (a directory) are dropped.
pub(super) fn clean(raw: &str) -> Result<Option<String>, String> {
    if raw.len() > MAX_PATH {
        return Err(format!("a path longer than {MAX_PATH} bytes"));
    }
    if raw.starts_with('/') {
        return Err("an absolute path".to_string());
    }
    let mut segments = vec![];
    let body = raw.strip_suffix('/').unwrap_or(raw);
    for segment in body.split('/') {
        match segment {
            "." => {}
            "" if segments.is_empty() && body.is_empty() => {}
            "" => return Err("an empty path segment (`//`)".to_string()),
            ".." => return Err("a `..` segment".to_string()),
            name => {
                check_segment(name)?;
                segments.push(name);
            }
        }
    }
    if segments.len() > MAX_DEPTH {
        return Err(format!("a path more than {MAX_DEPTH} directories deep"));
    }
    Ok((!segments.is_empty()).then(|| segments.join("/")))
}

/// `Err` unless `name` is a file name every OS holds the same way.
fn check_segment(name: &str) -> Result<(), String> {
    if let Some(c) = name.chars().find(|c| c.is_control()) {
        return Err(match c {
            '\0' => "a NUL in a name".to_string(),
            c => format!("a control character ({:?}) in a name", c),
        });
    }
    if let Some(c) = name.chars().find(|c| "\\:<>\"|?*".contains(*c)) {
        return Err(format!(
            "`{c}` in a name (Windows can't hold it{})",
            match c {
                '\\' => ", and reads it as a separator",
                ':' => ", and reads it as a drive or an alternate stream",
                _ => "",
            }
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(
            "a name ending in a dot or a space (Windows drops them, so it is another file there)"
                .to_string(),
        );
    }
    if is_dot_git(name) {
        return Err(
            "a `.git` directory or file (a git command run in the tree would read it: its config \
             can name commands to run)"
                .to_string(),
        );
    }
    if is_short_name(name) {
        return Err(format!(
            "`{name}`, a Windows short (8.3) name, which can be another file's second name there"
        ));
    }
    if is_device_name(name) {
        return Err(format!(
            "`{name}`, a device name on Windows (CON, NUL, COM1, …, with any extension)"
        ));
    }
    Ok(())
}

/// Whether `name` is `.git` as any filesystem might take it: in any case or Unicode
/// normalisation, and with the code points HFS+ ignores in names removed (`.g\u{200c}it` is
/// `.git` there), as git itself checks a tree's names. Its 8.3 form `GIT~1` is a short name, and
/// `.git.` and `.git ` end in a dot or a space: refused as those.
fn is_dot_git(name: &str) -> bool {
    git_key(name) == ".git"
}

/// `name` as git compares it on HFS+, NTFS and APFS: the code points HFS+ ignores removed, then
/// [`clash_key`].
fn git_key(name: &str) -> String {
    let kept: String = name.chars().filter(|c| !is_hfs_ignorable(*c)).collect();
    clash_key(&kept)
}

/// `Err` if a directory among `paths` (and the archive's root) would be a git repository by its
/// contents, whatever its name: git takes a directory holding `HEAD` with `objects` and `refs`,
/// or `HEAD` with `commondir`, for one, and a `git` command run there runs what its `config`
/// says (`core.fsmonitor`). Names compare as [`git_key`] folds them.
pub(super) fn check_no_repository<'a>(paths: impl Iterator<Item = &'a str>) -> Result<(), String> {
    const HEAD: u8 = 1;
    const OBJECTS: u8 = 2;
    const REFS: u8 = 4;
    const COMMONDIR: u8 = 8;
    let mut dirs: std::collections::HashMap<String, (u8, &str)> = Default::default();
    for path in paths {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        // Each of these names is ASCII, and a character folding to ASCII takes at most 3 bytes:
        // a longer name (ignorable code points apart) folds to none of them.
        let kept_len: usize = (name.chars())
            .filter(|c| !is_hfs_ignorable(*c))
            .map(char::len_utf8)
            .sum();
        if kept_len > 3 * "commondir".len() {
            continue;
        }
        let bit = match git_key(name).as_str() {
            "head" => HEAD,
            "objects" => OBJECTS,
            "refs" => REFS,
            "commondir" => COMMONDIR,
            _ => continue,
        };
        let key = dir.split('/').map(git_key).collect::<Vec<_>>().join("/");
        dirs.entry(key).or_insert((0, dir)).0 |= bit;
    }
    for (bits, dir) in dirs.values() {
        let has = |b: u8| bits & b == b;
        if has(HEAD) && (has(OBJECTS | REFS) || has(COMMONDIR)) {
            let at = match dir.is_empty() {
                true => "the archive's root".to_string(),
                false => format!("`{}`", dir.escape_debug()),
            };
            return Err(format!(
                "{at} holds `HEAD` with `objects` and `refs` (or `commondir`): git would take it for a repository and run what its `config` says"
            ));
        }
    }
    Ok(())
}

/// The code points HFS+ drops when it compares names (git's `is_hfs_dotgit` list).
fn is_hfs_ignorable(c: char) -> bool {
    matches!(c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}

/// A name in the form Windows gives long names as their second, short name: up to six characters,
/// `~`, digits, and an extension of up to three (`PROGRA~1`, `ABCDEF~1.TXT`).
fn is_short_name(name: &str) -> bool {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let Some((stem, digits)) = base.split_once('~') else {
        return false;
    };
    (1..=6).contains(&stem.chars().count())
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && ext.chars().count() <= 3
        && !ext.contains('.')
}

/// Windows' reserved device names, alone or before an extension, in any case.
fn is_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let (Some(prefix), Some(digit)) = (upper.get(..3), upper.get(3..)) else {
                return false;
            };
            matches!(prefix, "COM" | "LPT")
                && matches!(
                    digit,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
        }
    }
}

/// `Err` unless `target` is a link target that can stay inside the tree: relative, with no NUL,
/// backslash or drive. Where it leads is checked once every link is known (`Tree::finish`).
pub(super) fn check_link_target(target: &str) -> Result<(), String> {
    let why = if target.is_empty() {
        "a link with an empty target"
    } else if target.len() > MAX_PATH {
        "a link target longer than 4096 bytes"
    } else if target.starts_with('/') {
        "a link to an absolute path"
    } else if target.chars().any(|c| c.is_control()) {
        "a NUL or another control character in a link target"
    } else if target.contains('\\') || target.contains(':') {
        "a `\\` or `:` in a link target (a Windows path or drive)"
    } else {
        return Ok(());
    };
    Err(why.to_string())
}

/// The same string for two paths that macOS or Windows would take for one file: the path in
/// Unicode normalisation form C, case-folded. Uppercasing before lowercasing folds what
/// lowercasing alone keeps apart (`ſ` and `s`, `ς` and `σ`, which APFS takes for one name), and
/// a second round folds what one round leaves apart (`ẞ` lowercases to `ß`, which uppercases to
/// `SS`). Writing never goes through what is on disk (`write_into`), so a fold this misses is
/// an error, not an escape.
pub(super) fn clash_key(path: &str) -> String {
    const NFC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfc();
    let once = NFC.normalize(path).to_uppercase().to_lowercase();
    let twice = once.to_uppercase().to_lowercase();
    NFC.normalize(&twice).into_owned()
}
