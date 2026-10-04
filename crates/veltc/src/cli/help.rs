//! Help text for `velt --help` and `velt <command> --help`, from one table of commands that also
//! feeds shell completions ([`super::completions`]) and "did you mean" for options.

use crate::style::{paint, Stream, Style};
use crate::templates::Template;

/// One `velt` command's help.
pub struct CommandHelp {
    /// The command word.
    pub name: &'static str,
    /// One line for the command list.
    pub summary: &'static str,
    /// Usage lines, each after `velt `.
    pub usage: &'static [&'static str],
    /// Longer explanation (may be empty).
    pub about: &'static str,
    /// `(label, description)`; the label's words starting with `-` are the flags.
    pub options: &'static [(&'static str, &'static str)],
    /// `(command line, what it does)`.
    pub examples: &'static [(&'static str, &'static str)],
}

impl CommandHelp {
    /// Every flag the command accepts (`-o`, `--output`, …), `-h`/`--help` included.
    pub fn flags(&self) -> Vec<&'static str> {
        let mut flags: Vec<&'static str> = self
            .options
            .iter()
            .flat_map(|(label, _)| label.split([' ', ',']))
            .filter(|w| w.starts_with('-'))
            .collect();
        flags.extend(["-h", "--help"]);
        flags
    }
}

const RELEASE: (&str, &str) = (
    "--release",
    "optimize (LLVM when clang is found) and link without debug info",
);
const DEBUG_INFO: (&str, &str) = ("-g", "keep debug info in a --release build");
const TARGET: (&str, &str) = (
    "--target <triple>",
    "target triple (default: the host); wasm32-wasip1 and wasm32-unknown-unknown build WebAssembly",
);
const BACKEND: (&str, &str) = (
    "--backend <name>",
    "cranelift | llvm (default: llvm for --release when clang is found)",
);
const LOCKED: (&str, &str) = ("--locked", "fail if velt.lock.json would change");
const VERBOSE: (&str, &str) = ("-v, --verbose", "print per-stage timings to stderr");
const TIMINGS: (&str, &str) = (
    "--timings",
    "like -v, plus the time of each optimizer pass and codegen step",
);
const TEMPLATE: (&str, &str) = (
    "--template <name>",
    "app | cli | api | websocket | lib (default: app)",
);

/// Every command, in the order `velt --help` lists them.
pub const COMMANDS: &[CommandHelp] = &[
    CommandHelp {
        name: "new",
        summary: "Create a package in a new directory from a template",
        usage: &["new <name> [--template <name>] [--lib]"],
        about: "Creates <name>/ with package.vlt, .gitignore, README.md, src/ and tests/. Every \
                template builds, passes `velt test` and is formatted.",
        options: &[TEMPLATE, ("--lib", "same as --template lib")],
        examples: &[
            ("velt new hello", "hello world with a test"),
            ("velt new todo --template api", "JSON HTTP API with tests against a live server"),
            ("velt new tool --template cli", "command-line tool with subcommands and --help"),
            ("velt new textkit --template lib", "library with doc comments and tests"),
        ],
    },
    CommandHelp {
        name: "init",
        summary: "Turn the current directory into a package from a template",
        usage: &["init [--template <name>] [--name <name>] [--force]"],
        about: "Like `velt new`, in the current directory. The package is named after the \
                directory unless --name is given. Existing files are never overwritten without \
                --force (an existing README.md is kept and `target/` is added to an existing \
                .gitignore).",
        options: &[
            TEMPLATE,
            ("--name <name>", "package name (default: the directory's name)"),
            ("--force", "overwrite files the template would create"),
        ],
        examples: &[
            ("velt init", "hello world in the current directory"),
            ("velt init --template websocket --name chat", "WebSocket chat, package `chat`"),
        ],
    },
    CommandHelp {
        name: "build",
        summary: "Compile a file or the current package",
        usage: &["build [<file.vlt>] [-o <out>] [--release] [-g] [--target <triple>] [--backend <name>] [--emit <kind>] [--locked] [-v] [--timings]"],
        about: "Without a file, builds the package found by searching upward for package.vlt \
                (output: <package>/target/velt/<name>[.exe]). A single file builds to \
                ./target/velt/<stem>[.exe].",
        options: &[
            ("-o, --output <path>", "output file"),
            RELEASE,
            DEBUG_INFO,
            TARGET,
            BACKEND,
            ("--emit <kind>", "vir | llvm (print VIR / LLVM IR and stop) | obj (object file only) | exe"),
            LOCKED,
            VERBOSE,
            TIMINGS,
        ],
        examples: &[
            ("velt build", "build the current package"),
            ("velt build --release", "optimized build"),
            ("velt build hello.vlt -o hello", "build one file"),
            ("velt build app.vlt --target wasm32-wasip1", "build WebAssembly"),
        ],
    },
    CommandHelp {
        name: "run",
        summary: "Build and run a file or the current package",
        usage: &[
            "run [--release] [-g] [--target <triple>] [--backend <name>] [--locked] [-v] <file.vlt> [<program args>...]",
            "run [--release] [-g] [--target <triple>] [--backend <name>] [--locked] [-v] [-- <program args>...]",
        ],
        about: "Exits with the program's exit code. Arguments after the file (or after `--`) go \
                to the program; Velt's options come before the file.",
        options: &[RELEASE, DEBUG_INFO, TARGET, BACKEND, LOCKED, VERBOSE],
        examples: &[
            ("velt run", "run the current package"),
            ("velt run hello.vlt", "run one file"),
            ("velt run server.vlt --port 8080", "pass arguments to the program"),
            ("velt run -- --port 8080", "pass arguments to the package's program"),
        ],
    },
    CommandHelp {
        name: "check",
        summary: "Type-check a file or the current package without building it",
        usage: &[
            "check [<file.vlt>] [--json] [--locked] [-v]",
            "check --ts-compat [<file|dir>...] [--json] [--locked] [-v]",
        ],
        about: "Parses and type-checks like `velt build`, prints the diagnostics and exits \
                with 1 if there are errors. Nothing is lowered, compiled or linked. With a \
                file, checks it and every file it imports; a library module needs no `main`. \
                Without one, checks every `.vlt` module under `src/` and `tests/` of the \
                current package; its entry must define `main` (a library package's root is \
                `src/lib.vlt`, with no `main`). With `--ts-compat`, checks the given files \
                (directories: their `.vlt`, `.ts` and `.tsx` files), or without paths the \
                folders the package's `tsCompat` lists, then reports what in them `tsc` would \
                reject or run differently, for code shared with TypeScript.",
        options: &[
            ("--json", "diagnostics as one JSON document on stdout (for editors and tools)"),
            (
                "--ts-compat [<file|dir>...]",
                "lint the given files and directories (default: the package's `tsCompat` \
                 folders) for the TypeScript/Velt common subset",
            ),
            LOCKED,
            VERBOSE,
        ],
        examples: &[
            ("velt check", "check the current package"),
            ("velt check app.vlt --json", "machine-readable diagnostics for one file"),
            ("velt check --ts-compat src/models", "check code shared with a TypeScript client"),
            ("velt check --ts-compat", "lint the folders `tsCompat` lists in package.vlt"),
        ],
    },
    CommandHelp {
        name: "dev",
        summary: "Run, then rebuild and restart on every change",
        usage: &["dev [<file.vlt>] [--exe] [--locked] [-v] [--timings] [-- <program args>...]"],
        about: "Runs the program like `velt run`, then rebuilds and restarts it whenever a file \
                it imports (or package.vlt/velt.lock.json) changes; a failed build leaves the old \
                version running. The program runs JIT-compiled inside `velt`, and listening \
                sockets stay open across restarts.",
        options: &[
            ("--exe", "link an executable for each version instead of the JIT"),
            RELEASE,
            DEBUG_INFO,
            BACKEND,
            LOCKED,
            VERBOSE,
            TIMINGS,
        ],
        examples: &[
            ("velt dev", "develop the current package"),
            ("velt dev server.vlt -- --port 8080", "one file, with program arguments"),
        ],
    },
    CommandHelp {
        name: "test",
        summary: "Run the tests (`export function test_*` in *.test.vlt, .ts, .tsx)",
        usage: &["test [<file|dir>] [--release] [--locked] [--watch]"],
        about: "Every `export function test_*()` (or `export async function test_*()`) in a \
                *.test.vlt (or .test.ts, .test.tsx) file is a test. Prints `ok <name>` / `FAILED <name>` and exits \
                with 1 if any test failed.",
        options: &[
            ("--release", "build the tests optimized"),
            LOCKED,
            ("--watch", "rerun on every change"),
        ],
        examples: &[
            ("velt test", "every test of the package (or under the current directory)"),
            ("velt test tests/api.test.vlt", "one file"),
            ("velt test --watch", "rerun on every change"),
        ],
    },
    CommandHelp {
        name: "fmt",
        summary: "Format .vlt (and .ts, .tsx) files",
        usage: &["fmt [<file|dir>...] [--check]"],
        about: "Without paths, formats the package's package.vlt and the .vlt, .ts and .tsx \
                files of src/ (or every .vlt file under the current directory).",
        options: &[("--check", "write nothing; list unformatted files and exit 1 if any")],
        examples: &[
            ("velt fmt", "format the package"),
            ("velt fmt --check src tests", "check formatting (CI)"),
        ],
    },
    CommandHelp {
        name: "clean",
        summary: "Remove the package's target/ directory",
        usage: &["clean"],
        about: "Deletes <package>/target (build outputs, test binaries, docs) and reports the \
                space freed.",
        options: &[],
        examples: &[("velt clean", "remove the build outputs of the current package")],
    },
    CommandHelp {
        name: "add",
        summary: "Add a dependency to package.vlt and install it",
        usage: &["add <pkg>[@<req>] [--path <dir>]"],
        about: "Without a version requirement, uses the latest published version.",
        options: &[("--path <dir>", "a local package instead of a registry one")],
        examples: &[
            ("velt add json", "latest version from the registry"),
            ("velt add json@^1.2", "a version requirement"),
            ("velt add util --path ../util", "a package in a local directory"),
        ],
    },
    CommandHelp {
        name: "install",
        summary: "Resolve and fetch dependencies, write velt.lock.json",
        usage: &["install [--locked]"],
        about: "",
        options: &[LOCKED],
        examples: &[("velt install --locked", "install exactly what velt.lock.json says")],
    },
    CommandHelp {
        name: "update",
        summary: "Re-resolve dependencies ignoring velt.lock.json",
        usage: &["update"],
        about: "",
        options: &[],
        examples: &[("velt update", "pick the newest matching versions")],
    },
    CommandHelp {
        name: "publish",
        summary: "Publish the package to the registry",
        usage: &["publish [--native-artifacts <dir>] [--native-only]"],
        about: "Publishes to the package's `registry`, else $VELT_REGISTRY, else the local \
                registry (~/.velt/registry). A package with `native` in package.vlt also publishes a \
                prebuilt native library for every target it lists: from <dir>/<triple>/, else \
                target/velt-native/<triple>/ (the host's is built if missing).",
        options: &[
            ("--native-artifacts <dir>", "native libraries built elsewhere (`velt native build` on each OS)"),
            ("--native-only", "add libraries for new targets to the published version"),
        ],
        examples: &[
            ("velt publish", "publish the current package"),
            ("velt publish --native-artifacts dist", "with libraries built by CI for each target"),
        ],
    },
    CommandHelp {
        name: "manifest",
        summary: "Check the package's manifest, or print it as JSON",
        usage: &["manifest [--json]"],
        about: "Reads package.vlt the way every command does (as data, without running anything) \
                and reports its errors. With --json it prints the manifest as JSON with the \
                defaults filled in, for tools that cannot read Velt.",
        options: &[("--json", "print the manifest as JSON on stdout")],
        examples: &[
            ("velt manifest", "check package.vlt"),
            ("velt manifest --json", "the current package's manifest as JSON"),
        ],
    },
    CommandHelp {
        name: "native",
        summary: "Build the package's native library",
        usage: &["native build [--target <triple>]"],
        about: "Builds the `native` crate with cargo and writes the bundle `velt publish` uploads \
                to target/velt-native/<triple>/ (needs Rust; users of the package do not).",
        options: &[("--target <triple>", "the target to build for (default: this machine)")],
        examples: &[(
            "velt native build --target aarch64-apple-darwin",
            "the macOS arm64 library",
        )],
    },
    CommandHelp {
        name: "search",
        summary: "Find packages in the registry",
        usage: &["search <text> [--json]"],
        about: "Lists the packages matching every word of the text in their name, keywords or \
                description (name matches first), with their newest version that is not yanked \
                and its description, from the package's registry (or $VELT_REGISTRY).",
        options: &[("--json", "the registry's answer as JSON on stdout")],
        examples: &[
            ("velt search json", "packages about JSON"),
            ("velt search json parser --json", "for scripts"),
        ],
    },
    CommandHelp {
        name: "login",
        summary: "Store your token for a registry server",
        usage: &["login <registry-url>"],
        about: "Reads the token (from `velt registry user add` on the server) from stdin and \
                stores it in $VELT_HOME/credentials.json, readable only by you. It is sent only \
                to that registry, and never over plain http:// to another machine. \
                $VELT_REGISTRY_TOKEN overrides it (for CI).",
        options: &[],
        examples: &[
            ("velt login https://registry.example.com", "paste the token when asked"),
            ("echo $TOKEN | velt login https://registry.example.com", "in a script"),
        ],
    },
    CommandHelp {
        name: "logout",
        summary: "Forget your token for a registry server",
        usage: &["logout <registry-url>"],
        about: "Removes the registry's token from $VELT_HOME/credentials.json.",
        options: &[],
        examples: &[("velt logout https://registry.example.com", "remove its token")],
    },
    CommandHelp {
        name: "yank",
        summary: "Withdraw a published version (or bring it back)",
        usage: &["yank <pkg>@<version> [--undo]"],
        about: "A yanked version is never chosen for a new dependency, but projects whose \
                velt.lock.json pins it keep installing it. Only the package's owners may yank.",
        options: &[("--undo", "unyank the version")],
        examples: &[
            ("velt yank json@1.2.0", "withdraw json 1.2.0"),
            ("velt yank json@1.2.0 --undo", "bring it back"),
        ],
    },
    CommandHelp {
        name: "owner",
        summary: "List or change a package's owners",
        usage: &["owner list <pkg>", "owner add <pkg> <user>", "owner remove <pkg> <user>"],
        about: "Owners publish versions, yank and change the owners of a package on a registry \
                server; its first publisher is its first owner. The last owner can't be removed.",
        options: &[],
        examples: &[
            ("velt owner list json", "who may publish json"),
            ("velt owner add json bob", "let bob publish json too"),
        ],
    },
    CommandHelp {
        name: "doc",
        summary: "Generate HTML API docs",
        usage: &["doc [<file|dir>...] [--std] [-o <dir>]"],
        about: "Documents exported items and the `///` comments above them. Without paths: the \
                package's src/ into <package>/target/doc.",
        options: &[
            ("--std", "document the standard library"),
            ("-o, --output <dir>", "output directory"),
        ],
        examples: &[
            ("velt doc", "docs for the package"),
            ("velt doc --std -o std-docs", "standard library docs"),
        ],
    },
    CommandHelp {
        name: "lsp",
        summary: "Language server on stdin/stdout (for editors)",
        usage: &["lsp [--stdio]"],
        about: "",
        options: &[("--stdio", "accepted for editor compatibility (stdio is the only transport)")],
        examples: &[("velt lsp", "started by the VS Code extension")],
    },
    CommandHelp {
        name: "playground",
        summary: "Write and run programs in the browser",
        usage: &["playground [--port <n>] [--host <addr>]"],
        about: "",
        options: &[
            ("--port <n>", "port (default 8090)"),
            ("--host <addr>", "address (default 127.0.0.1)"),
        ],
        examples: &[("velt playground --port 9000", "serve on http://127.0.0.1:9000")],
    },
    CommandHelp {
        name: "registry",
        summary: "Serve a package registry over HTTP",
        usage: &[
            "registry serve [--dir <d>] [--port <n>] [--host <addr>]",
            "registry user add|remove|token <name> [--dir <d>] [--open]",
            "registry owner add|remove <pkg> <user> [--dir <d>]",
        ],
        about: "A registry with users needs a user's token for every write (publishing, yanking, \
                owners), and only a package's owners may change it; without users anyone who \
                can reach the server may publish. `registry user add` and `token` print the \
                user's new token once; the user stores it with `velt login <url>` (or sets \
                $VELT_REGISTRY_TOKEN in CI). Removing the \
                last user needs --open. `registry owner` assigns owners directly (for packages \
                that have none). The server speaks plain HTTP: beyond localhost, put it behind a \
                TLS reverse proxy.",
        options: &[
            ("--dir <d>", "registry directory (default: the local registry)"),
            ("--port <n>", "port (default 8091)"),
            ("--host <addr>", "address (default 127.0.0.1)"),
            ("--open", "let `user remove` delete the last user (the registry becomes open)"),
        ],
        examples: &[
            ("velt registry serve --dir ./registry", "share a directory of packages"),
            ("velt registry owner add json alice --dir ./registry", "give `json` an owner"),
            ("velt registry user add alice --dir ./registry", "a user, and the token for them"),
        ],
    },
    CommandHelp {
        name: "doctor",
        summary: "Check the installation and run a hello world",
        usage: &["doctor"],
        about: "Checks the runtime library, std, linker and clang, then builds and runs a hello \
                world. Exit code 0 when every required check passes.",
        options: &[],
        examples: &[("velt doctor", "check the toolchain")],
    },
    CommandHelp {
        name: "completions",
        summary: "Print a shell completion script",
        usage: &["completions <bash|zsh|fish|powershell>"],
        about: "Completes commands, options, template names and files.",
        options: &[],
        examples: &[
            ("velt completions bash > ~/.local/share/bash-completion/completions/velt", "bash"),
            ("velt completions zsh > ~/.zfunc/_velt", "zsh (with ~/.zfunc on $fpath)"),
            ("velt completions fish > ~/.config/fish/completions/velt.fish", "fish"),
            ("velt completions powershell >> $PROFILE", "PowerShell"),
        ],
    },
    CommandHelp {
        name: "help",
        summary: "Show help for a command",
        usage: &["help [<command>]"],
        about: "",
        options: &[],
        examples: &[("velt help build", "same as `velt build --help`")],
    },
];

/// The help entry of command `name`.
pub fn find(name: &str) -> Option<&'static CommandHelp> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// Every command name.
pub fn command_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|c| c.name).collect()
}

const ENVIRONMENT: &[(&str, &str)] = &[
    ("VELT_STD", "standard library directory"),
    ("VELT_HOME", "vpm home (default ~/.velt: cache/, registry/)"),
    (
        "VELT_REGISTRY",
        "package registry: a directory (default $VELT_HOME/registry) or an http(s):// URL",
    ),
    (
        "VELT_REGISTRY_TOKEN",
        "a registry token for every registry server, overriding `velt login` (for CI)",
    ),
    (
        "VELT_CA_FILE",
        "PEM file of extra CA certificates to trust for https:// registries",
    ),
    (
        "VELT_CLANG",
        "clang used by the LLVM backend (default: PATH, then the standard LLVM install)",
    ),
    (
        "VELT_RT_LIB",
        "runtime library (default: next to velt, or <prefix>/lib when installed)",
    ),
    (
        "VELT_RT_LINK",
        "`static`: debug builds link the static runtime instead of the shared one",
    ),
    ("VELT_LINKER", "linker program override"),
    (
        "VELT_LLVM_BIN",
        "LLVM bin directory with opt/llc for WebAssembly (default: rustup's llvm-tools)",
    ),
    (
        "VELT_WASI_SYSROOT",
        "directory with wasi-libc's crt1-command.o and libc.a (default: rustup's)",
    ),
    (
        "VELT_WASM_RUNNER",
        "program running wasm32-wasip1 modules for `velt run` (default: wasmtime)",
    ),
    ("NO_COLOR", "set to disable colored output"),
];

fn heading(text: &str) -> String {
    paint(Stream::Stdout, Style::Heading, text)
}

fn literal(text: &str) -> String {
    paint(Stream::Stdout, Style::Literal, text)
}

/// `label` padded to `width`, then `desc` (the label colored after padding, so escapes don't
/// count toward the width).
fn row(label: &str, width: usize, desc: &str) -> String {
    let pad = " ".repeat(width.saturating_sub(label.chars().count()) + 2);
    format!("  {}{pad}{desc}\n", literal(label))
}

/// `velt --help`: the command list, global options and environment.
pub fn overview() -> String {
    let mut out = format!(
        "The Velt toolchain: compiler, runner, test runner, formatter and package manager.\n\n{} velt <command> [options]\n\n{}\n",
        heading("usage:"),
        heading("commands:")
    );
    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in COMMANDS {
        out.push_str(&row(c.name, width, c.summary));
    }
    out.push_str(&format!("\n{}\n", heading("options:")));
    out.push_str(&row(
        "-h, --help",
        13,
        "print help (`velt <command> --help` for a command)",
    ));
    out.push_str(&row("-V, --version", 13, "print the version"));
    out.push_str(&format!(
        "\n{}\n",
        heading("templates (velt new --template <name>):")
    ));
    for t in Template::ALL {
        out.push_str(&row(t.name(), 9, t.description()));
    }
    out.push_str(&format!("\n{}\n", heading("environment:")));
    let width = ENVIRONMENT.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, desc) in ENVIRONMENT {
        out.push_str(&row(name, width, desc));
    }
    out.push_str(&format!(
        "\nGet started: {}",
        literal("velt new hello && cd hello && velt run")
    ));
    out
}

/// `velt <command> --help`.
pub fn command(c: &CommandHelp) -> String {
    let mut out = format!("{}\n\n{}", c.summary, heading("usage:"));
    for u in c.usage {
        out.push_str(&format!("\n  velt {u}"));
    }
    out.push('\n');
    if !c.about.is_empty() {
        out.push_str(&format!("\n{}\n", wrap(c.about, 96)));
    }
    let mut options = c.options.to_vec();
    options.push(("-h, --help", "print this help"));
    let width = options.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    out.push_str(&format!("\n{}\n", heading("options:")));
    for (label, desc) in &options {
        out.push_str(&row(label, width, desc));
    }
    if c.name == "new" || c.name == "init" {
        out.push_str(&format!("\n{}\n", heading("templates:")));
        for t in Template::ALL {
            out.push_str(&row(t.name(), 9, t.description()));
        }
    }
    out.push_str(&format!("\n{}\n", heading("examples:")));
    for (cmd, desc) in c.examples {
        out.push_str(&format!("  {}\n      {desc}\n", literal(cmd)));
    }
    out.truncate(out.trim_end().len());
    out
}

/// Greedy word wrap at `width` columns.
fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line_len = 0;
    for word in text.split_whitespace() {
        if line_len > 0 && line_len + 1 + word.len() > width {
            out.push('\n');
            line_len = 0;
        } else if line_len > 0 {
            out.push(' ');
            line_len += 1;
        }
        out.push_str(word);
        line_len += word.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_usage_and_examples() {
        for c in COMMANDS {
            assert!(!c.usage.is_empty(), "{}", c.name);
            assert!(!c.examples.is_empty(), "{} has no examples", c.name);
            assert!(c.usage.iter().all(|u| u.starts_with(c.name)), "{}", c.name);
            let text = command(c);
            assert!(
                text.contains("usage:") && text.contains("examples:"),
                "{text}"
            );
            assert!(text.contains("-h, --help"), "{text}");
        }
    }

    #[test]
    fn overview_lists_every_command_and_template() {
        let text = overview();
        for c in COMMANDS {
            assert!(text.contains(c.summary), "{}", c.name);
        }
        for t in Template::ALL {
            assert!(text.contains(t.description()));
        }
        assert!(text.contains("NO_COLOR"));
    }

    #[test]
    fn flags_come_from_option_labels() {
        let build = find("build").unwrap();
        let flags = build.flags();
        for f in [
            "-o",
            "--output",
            "--release",
            "-g",
            "--emit",
            "-v",
            "--verbose",
            "--help",
        ] {
            assert!(flags.contains(&f), "{f} missing from {flags:?}");
        }
        assert!(!flags.contains(&"<path>"));
    }

    #[test]
    fn wraps_long_text() {
        assert_eq!(wrap("aa bb cc", 5), "aa bb\ncc");
    }
}
