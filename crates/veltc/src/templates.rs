//! Project templates for `velt new` / `velt init`, embedded in the binary. The sources live in
//! `crates/veltc/templates/<template>/` as ordinary, formatted Velt projects (minus `velt.toml`
//! and `.gitignore`, which are generated); `{{name}}` in a file stands for the package name.
//! `tests/templates.rs` builds, tests and format-checks every template.

use vpm::scaffold::{IfExists, ScaffoldFile, GITIGNORE};

/// A project template.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Template {
    /// Hello world with a module and a test (the default).
    #[default]
    App,
    /// Command-line tool: std/cli argument parsing, subcommands, `--help`.
    Cli,
    /// JSON HTTP API: routes, validation, typed errors → status codes.
    Api,
    /// WebSocket chat server and client.
    Websocket,
    /// Library: exports, doc comments, tests.
    Lib,
}

/// One embedded file: path relative to the package root, and its text.
type Embedded = (&'static str, &'static str);

macro_rules! embed {
    ($template:literal: $($path:literal),+ $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/", $template, "/", $path)))),+]
    };
}

const APP: &[Embedded] =
    embed!("app": "README.md", "src/main.vlt", "src/greet.vlt", "tests/greet.test.vlt");
const CLI: &[Embedded] = embed!("cli":
    "README.md", "src/main.vlt", "src/commands.vlt", "tests/commands.test.vlt");
const API: &[Embedded] = embed!("api":
    "README.md", "src/main.vlt", "src/model.vlt", "src/store.vlt", "src/api.vlt",
    "src/server.vlt", "tests/api.test.vlt", "tests/model.test.vlt");
const WEBSOCKET: &[Embedded] = embed!("websocket":
    "README.md", "src/main.vlt", "src/protocol.vlt", "src/room.vlt", "src/server.vlt",
    "src/client.vlt", "tests/protocol.test.vlt", "tests/chat.test.vlt");
const LIB: &[Embedded] = embed!("lib": "README.md", "src/lib.vlt", "tests/lib.test.vlt");

impl Template {
    /// Every template, in the order `--help` lists them.
    pub const ALL: [Template; 5] = [
        Template::App,
        Template::Cli,
        Template::Api,
        Template::Websocket,
        Template::Lib,
    ];

    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Template::App => "app",
            Template::Cli => "cli",
            Template::Api => "api",
            Template::Websocket => "websocket",
            Template::Lib => "lib",
        }
    }

    /// One-line description.
    pub fn description(self) -> &'static str {
        match self {
            Template::App => "hello world with a module and a test (default)",
            Template::Cli => "command-line tool: std/cli, subcommands, --help",
            Template::Api => "JSON HTTP API: routes, validation, errors as status codes",
            Template::Websocket => "WebSocket chat server and terminal client",
            Template::Lib => "library: exports, doc comments for `velt doc`, tests",
        }
    }

    /// The template called `name` (`web-socket`/`ws` are accepted for `websocket`).
    pub fn parse(name: &str) -> Result<Template, String> {
        let found = match name {
            "ws" | "web-socket" => Some(Template::Websocket),
            "library" => Some(Template::Lib),
            _ => Template::ALL.into_iter().find(|t| t.name() == name),
        };
        found.ok_or_else(|| {
            let names: Vec<&str> = Template::ALL.iter().map(|t| t.name()).collect();
            let mut msg = format!("unknown template `{name}` (expected {})", names.join(", "));
            if let Some(s) = crate::cli::suggest::closest(name, &names) {
                msg.push_str(&format!("; did you mean `{s}`?"));
            }
            msg
        })
    }

    /// Whether the package is a library (no runnable entry).
    pub fn is_lib(self) -> bool {
        self == Template::Lib
    }

    fn embedded(self) -> &'static [Embedded] {
        match self {
            Template::App => APP,
            Template::Cli => CLI,
            Template::Api => API,
            Template::Websocket => WEBSOCKET,
            Template::Lib => LIB,
        }
    }

    /// The files of package `name` made from this template: `velt.toml`, `.gitignore` (merged
    /// into an existing one), `README.md` (an existing one is kept) and the sources.
    pub fn files(self, name: &str) -> Vec<ScaffoldFile> {
        let mut files = vec![
            ScaffoldFile::new(
                vpm::manifest::MANIFEST_FILE,
                vpm::scaffold::manifest_text(name),
            ),
            ScaffoldFile {
                if_exists: IfExists::AppendLines,
                ..ScaffoldFile::new(".gitignore", GITIGNORE.to_string())
            },
        ];
        for (path, text) in self.embedded() {
            let file = ScaffoldFile::new(path, text.replace("{{name}}", name));
            files.push(match *path {
                "README.md" => ScaffoldFile {
                    if_exists: IfExists::Keep,
                    ..file
                },
                _ => file,
            });
        }
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for t in Template::ALL {
            assert_eq!(Template::parse(t.name()), Ok(t));
        }
        assert_eq!(Template::parse("ws"), Ok(Template::Websocket));
        let err = Template::parse("apii").unwrap_err();
        assert!(err.contains("did you mean `api`?"), "{err}");
    }

    #[test]
    fn files_substitute_the_name_and_have_an_entry() {
        for t in Template::ALL {
            let files = t.files("demo-pkg");
            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            let entry = if t.is_lib() {
                "src/lib.vlt"
            } else {
                "src/main.vlt"
            };
            assert!(paths.contains(&entry), "{t:?}: {paths:?}");
            assert!(paths.iter().any(|p| p.ends_with(".test.vlt")), "{t:?}");
            assert!(
                files.iter().all(|f| !f.contents.contains("{{name}}")),
                "{t:?}"
            );
            let readme = files.iter().find(|f| f.path == "README.md").unwrap();
            assert!(readme.contents.starts_with("# demo-pkg\n"), "{t:?}");
        }
    }
}
