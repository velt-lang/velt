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
/// lowercasing alone keeps apart (`ſ` and `s`, `ς` and `σ`, which APFS takes for one name).
pub(super) fn clash_key(path: &str) -> String {
    const NFC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfc();
    let folded = NFC.normalize(path).to_uppercase().to_lowercase();
    NFC.normalize(&folded).into_owned()
}
