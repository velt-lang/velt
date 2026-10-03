//! `velt new` and `velt init`: a package from a [`Template`], in a new directory or the current
//! one, followed by a short "what next" list.

use std::path::Path;

use crate::style;
use crate::templates::Template;

/// `velt new <name> [--template <t>]` in the current directory.
pub fn new_package(name: &str, template: Template) -> Result<(), String> {
    let cwd = current_dir()?;
    vpm::scaffold::create_package(&cwd, name, &template.files(name))?;
    style::status(
        "Created",
        &format!(
            "{} package `{name}` ({} template)",
            kind(template),
            template.name()
        ),
    );
    next_steps(Some(name), template);
    Ok(())
}

/// `velt init [--template <t>] [--name <n>] [--force]`: make the current directory a package.
pub fn init_package(name: Option<&str>, template: Template, force: bool) -> Result<(), String> {
    let cwd = current_dir()?;
    let name = match name {
        Some(n) => n.to_string(),
        None => name_from_dir(&cwd)?,
    };
    vpm::scaffold::check_name(&name)?;
    if !force {
        if let Some(root) = vpm::manifest::find_package_root(&cwd).filter(|r| *r != cwd) {
            return Err(format!(
                "`{}` is already inside package `{}` (pass `--force` to create a nested package anyway)",
                cwd.display(),
                root.display()
            ));
        }
    }
    let written = vpm::scaffold::write_files(&cwd, &template.files(&name), force)?;
    style::status(
        "Initialized",
        &format!(
            "{} package `{name}` ({} template) in {}",
            kind(template),
            template.name(),
            cwd.display()
        ),
    );
    if !written.kept.is_empty() {
        eprintln!("             kept existing {}", written.kept.join(", "));
    }
    next_steps(None, template);
    Ok(())
}

fn current_dir() -> Result<std::path::PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))
}

fn kind(template: Template) -> &'static str {
    if template.is_lib() {
        "library"
    } else {
        "binary"
    }
}

/// The package name `velt init` derives from directory `dir`: lower-cased, every run of
/// characters a package name can't have (and of `-` and `_`) turned into one `-`, trimmed.
fn name_from_text(raw: &str) -> String {
    let mut name = String::new();
    // The current run of separators: a lone `-` or `_` is kept, any other run becomes one `-`.
    let mut run = String::new();
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if !name.is_empty() && !run.is_empty() {
                name.push_str(if run == "_" { "_" } else { "-" });
            }
            run.clear();
            name.push(c);
        } else {
            run.push(c);
        }
    }
    name
}

fn name_from_dir(dir: &Path) -> Result<String, String> {
    let raw = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = name_from_text(&raw);
    vpm::scaffold::check_name(&name).map_err(|_| {
        format!("cannot derive a package name from the directory `{raw}`; pass one with `--name <name>`")
    })?;
    Ok(name)
}

/// Print the commands to try next (to stderr, like the status lines).
fn next_steps(dir: Option<&str>, template: Template) {
    let mut steps: Vec<String> = dir.map(|d| format!("cd {d}")).into_iter().collect();
    match template {
        Template::Lib => steps.extend(["velt test".into(), "velt doc".into()]),
        Template::Cli => steps.extend(["velt run -- --help".into(), "velt test".into()]),
        Template::Websocket => steps.extend(["velt run -- serve".into(), "velt test".into()]),
        Template::App | Template::Api => steps.extend(["velt run".into(), "velt test".into()]),
    }
    let cmds: Vec<String> = steps
        .iter()
        .map(|s| style::paint(style::Stream::Stderr, style::Style::Literal, s))
        .collect();
    eprintln!("\nNext: {}", cmds.join(" && "));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_directories() {
        assert_eq!(name_from_dir(Path::new("/x/my-app")).unwrap(), "my-app");
        assert_eq!(name_from_dir(Path::new("/x/My App")).unwrap(), "my-app");
        assert_eq!(name_from_dir(Path::new("/x/tool_2")).unwrap(), "tool_2");
        // Runs of separators collapse to one `-`, and none is left at either end.
        assert_eq!(
            name_from_dir(Path::new("/x/my project (2)")).unwrap(),
            "my-project-2"
        );
        assert_eq!(name_from_dir(Path::new("/x/foo_ bar")).unwrap(), "foo-bar");
        assert_eq!(name_from_dir(Path::new("/x/_a--b__c-")).unwrap(), "a-b-c");
        assert!(name_from_dir(Path::new("/x/2fast"))
            .unwrap_err()
            .contains("--name"));
    }
}
