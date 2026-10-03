//! Executing parsed `velt` commands: [`build`] (build/run), [`check`] (front end only),
//! [`create`] (new/init from templates), [`package`] (add/install/update/publish), [`clean`],
//! [`test`] (the test runner), [`fmt`] (the formatter), [`lsp`] (the language server), [`doctor`]
//! (installation checks), [`version`] (`--version`); [`project`] finds and installs the enclosing
//! package; [`wasm`] runs WebAssembly builds. `velt dev` lives in [`crate::dev`]; help and
//! completions come from [`crate::cli`].

mod build;
mod check;
mod clean;
mod create;
mod doc;
mod doctor;
mod fmt;
pub mod lsp;
mod package;
mod project;
mod registry;
pub mod test;
mod version;
mod wasm;

use std::process::ExitCode;

use crate::cli::{self, Command, DevMode};
use crate::style;

pub use build::{build_options, exit_code, failure_code, report};

/// Run `cmd` and return the process exit code (`velt run` exits directly with the program's code).
pub fn execute(cmd: Command) -> ExitCode {
    let result = match cmd {
        Command::Version => {
            println!("{}", version::version_line());
            Ok(())
        }
        Command::Help(topic) => {
            match topic.as_deref().and_then(cli::help::find) {
                Some(c) => println!("{}", cli::help::command(c)),
                None => println!("{}", cli::help::overview()),
            }
            Ok(())
        }
        Command::Completions(shell) => {
            print!("{}", cli::completions::script(shell));
            Ok(())
        }
        Command::Clean => clean::clean_command(),
        Command::Build(args) => return build::build_command(&args),
        Command::Run {
            build: args,
            args: prog_args,
        } => return build::run_command(&args, &prog_args),
        Command::Check(args) => return check::check_command(&args),
        Command::Dev(args) if args.mode == DevMode::Host => return crate::dev::host_command(&args),
        Command::Dev(args) => return crate::dev::dev_command(args),
        Command::Test {
            path,
            release,
            locked,
            watch,
        } => return test::test_command(path.as_deref(), release, locked, watch),
        Command::Fmt { paths, check } => return fmt::fmt_command(&paths, check),
        Command::Lsp => lsp::lsp_command(),
        Command::Doc(args) => doc::doc_command(&args),
        Command::RegistryServe(args) => registry::serve_command(&args),
        Command::Playground(args) => return crate::playground::playground_command(&args),
        Command::Doctor => return doctor::doctor_command(),
        Command::New { name, template } => create::new_package(&name, template),
        Command::Init {
            name,
            template,
            force,
        } => create::init_package(name.as_deref(), template, force),
        Command::Add {
            name,
            version,
            path,
        } => package::add(&name, version, path),
        Command::Install { locked } => package::install(vpm::InstallOptions {
            locked,
            update: false,
            target: Some(velt_codegen_cl::host_triple()),
        }),
        Command::Update => package::install(vpm::InstallOptions {
            locked: false,
            update: true,
            target: Some(velt_codegen_cl::host_triple()),
        }),
        Command::Publish {
            native_artifacts,
            native_only,
        } => package::publish(native_artifacts.as_deref(), native_only),
        Command::NativeBuild { target } => package::native_build(target),
        Command::Manifest { json } => package::manifest(json),
        Command::Yank {
            name,
            version,
            undo,
        } => registry::yank_command(&name, &version, undo),
        Command::Owner { action, package } => registry::owner_command(&action, &package),
        Command::Search { query, json } => registry::search_command(&query, json),
        Command::Login { url } => registry::login_command(&url),
        Command::Logout { url } => registry::logout_command(&url),
        Command::RegistryOwner {
            add,
            package,
            user,
            dir,
        } => registry::admin_owner_command(add, &package, &user, dir.as_ref()),
        Command::RegistryUser { action, name, dir } => {
            registry::user_command(action, &name, dir.as_ref())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            style::error(&msg);
            ExitCode::from(1)
        }
    }
}
