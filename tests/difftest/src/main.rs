//! `difftest`: differential tester for the Velt compiler. Programs of the TypeScript/Velt shared
//! subset run under Node (the oracle) and under `velt` in every build mode; any difference in
//! stdout or exit code is a compiler bug. See `README.md` for usage and the subset's rules.

mod campaign;
mod check;
mod exec;
mod gen;
mod pool;
mod report;
mod runner;
mod shrink;
mod tsify;

use check::{Config, Mode, Oracle};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage:
  difftest run <file|dir>...          check programs (every .vlt file, `_*` helpers skipped)
  difftest gen <seed>                 print the generated program for a seed
  difftest fuzz --seeds A..B          generate + check seeds A..B-1, group failures, shrink one per group
  difftest shrink <file>              minimize a failing program (keeps its verdict)
options:
  --velt <path>       velt binary (default: <repo>/target/debug/velt)
  --node <path>       node binary (default: node on PATH)
  --modes <list>      comma-separated: debug,release,release-cl,fmt (default: the first three)
  -j, --jobs <n>      parallel workers (default: available cores)
  --out <dir>         where failing cases go (default: tests/difftest/out)
  --timeout <secs>    per-run budget for programs (default: 10; builds get 6x)
  --seeds <A..B>      seed range for `fuzz` (default: 0..100)
  --oracle <name>     node (default) or debug: compare the release modes with the debug build
  --wild              `gen`/`fuzz`: full-range wrapping integers and casts (implies --oracle debug)
  --std               `gen`/`fuzz`: random inputs through the std modules (url, encoding, crypto,
                      csv, regex, datetime), checked against Node's implementations";

/// Parsed command line.
struct Args {
    command: String,
    positional: Vec<String>,
    cfg: Config,
    jobs: usize,
    out: PathBuf,
    seeds: String,
    gen: gen::Options,
}

fn main() -> ExitCode {
    match parse(std::env::args().skip(1).collect()).and_then(dispatch) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("difftest: {e}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(args: Args) -> Result<ExitCode, String> {
    match args.command.as_str() {
        "run" => {
            let paths: Vec<PathBuf> = args.positional.iter().map(PathBuf::from).collect();
            let bugs = runner::run(&args.cfg, &paths, args.jobs, &args.out)?;
            Ok(if bugs == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        "gen" => {
            let seed = args.positional.first().ok_or("gen needs a seed")?;
            print!("{}", gen::program(parse_num(seed)? as u64, args.gen));
            Ok(ExitCode::SUCCESS)
        }
        "fuzz" => {
            let seeds = campaign::parse_seeds(&args.seeds)?;
            let bugs = campaign::fuzz(&args.cfg, args.gen, &seeds, args.jobs, &args.out)?;
            Ok(if bugs == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        "shrink" => {
            let file = args.positional.first().ok_or("shrink needs a file")?;
            campaign::shrink_file(&args.cfg, std::path::Path::new(file), &args.out)?;
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

fn parse(argv: Vec<String>) -> Result<Args, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut args = Args {
        command: String::new(),
        positional: Vec::new(),
        cfg: Config {
            velt: root
                .join("../../target/debug/velt")
                .with_extension(std::env::consts::EXE_EXTENSION),
            node: PathBuf::from("node"),
            build_timeout: Duration::from_secs(60),
            run_timeout: Duration::from_secs(10),
            modes: Mode::ALL.to_vec(),
            oracle: Oracle::Node,
        },
        jobs: pool::default_jobs(),
        out: root.join("out"),
        seeds: "0..100".into(),
        gen: gen::Options::default(),
    };
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("`{a}` needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--velt" => args.cfg.vlt = PathBuf::from(value()?),
            "--node" => args.cfg.node = PathBuf::from(value()?),
            "--modes" => args.cfg.modes = parse_modes(&value()?)?,
            "-j" | "--jobs" => args.jobs = parse_num(&value()?)?,
            "--out" => args.out = PathBuf::from(value()?),
            "--seeds" => args.seeds = value()?,
            "--oracle" => args.cfg.oracle = parse_oracle(&value()?)?,
            "--wild" => {
                args.gen.wild = true;
                args.cfg.oracle = Oracle::Debug;
            }
            "--std" => args.gen.std = true,
            "--timeout" => {
                let secs = parse_num(&value()?)? as u64;
                args.cfg.run_timeout = Duration::from_secs(secs);
                args.cfg.build_timeout = Duration::from_secs(secs * 6);
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ if a.starts_with('-') => return Err(format!("unknown option `{a}`\n{USAGE}")),
            _ if args.command.is_empty() => args.command = a,
            _ => args.positional.push(a),
        }
    }
    if args.command.is_empty() {
        return Err(USAGE.into());
    }
    Ok(args)
}

fn parse_oracle(name: &str) -> Result<Oracle, String> {
    match name {
        "node" => Ok(Oracle::Node),
        "debug" => Ok(Oracle::Debug),
        _ => Err(format!("unknown oracle `{name}` (node, debug)")),
    }
}

fn parse_modes(list: &str) -> Result<Vec<Mode>, String> {
    list.split(',')
        .map(|m| {
            Mode::KNOWN
                .into_iter()
                .find(|x| x.name() == m.trim())
                .ok_or_else(|| format!("unknown mode `{m}` (debug, release, release-cl, fmt)"))
        })
        .collect()
}

fn parse_num(s: &str) -> Result<usize, String> {
    s.parse().map_err(|_| format!("`{s}` is not a number"))
}
