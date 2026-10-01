//! `velt clean`: delete the package's `target/` directory (build outputs, test binaries, docs)
//! and report how much space that freed.

use std::path::Path;

use super::project::Project;
use crate::style;

/// `velt clean` in the package around the current directory.
pub fn clean_command() -> Result<(), String> {
    let root = Project::current_root()?;
    let target = root.join("target");
    if !target.is_dir() {
        style::status(
            "Clean",
            &format!("nothing to remove ({} does not exist)", target.display()),
        );
        return Ok(());
    }
    let (files, bytes) = measure(&target);
    std::fs::remove_dir_all(&target).map_err(|e| {
        format!(
            "cannot remove `{}`: {e} (is a program from it still running?)",
            target.display()
        )
    })?;
    style::status(
        "Removed",
        &format!(
            "{} ({files} file{}, {})",
            target.display(),
            if files == 1 { "" } else { "s" },
            human_bytes(bytes)
        ),
    );
    Ok(())
}

/// Number of files and their total size under `dir` (unreadable entries count as empty).
fn measure(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let (mut files, mut bytes) = (0, 0);
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            let (f, b) = measure(&entry.path());
            files += f;
            bytes += b;
        } else {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// `bytes` for people: `512 B`, `1.5 KiB`, `12.3 MiB`, `2.0 GiB`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_bytes(3 << 30), "3.0 GiB");
    }

    #[test]
    fn measures_trees() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        std::fs::write(tmp.path().join("a/x"), [0u8; 10]).unwrap();
        std::fs::write(tmp.path().join("a/b/y"), [0u8; 5]).unwrap();
        assert_eq!(measure(tmp.path()), (2, 15));
    }
}
