//! `velt-site`: build the docs website into a static directory.
//!
//! ```sh
//! cargo run -p velt_doc --bin velt-site -- [--out target/site] [--pages docs/site/pages.txt] [--std std]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut out = PathBuf::from("target/site");
    let mut pages = PathBuf::from("docs/site/pages.txt");
    let mut std_dir = PathBuf::from("std");
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--out" => &mut out,
            "--pages" => &mut pages,
            "--std" => &mut std_dir,
            _ => {
                eprintln!("usage: velt-site [--out <dir>] [--pages <pages.txt>] [--std <dir>]");
                return ExitCode::from(2);
            }
        };
        match args.next() {
            Some(v) => *slot = PathBuf::from(v),
            None => {
                eprintln!("error: `{flag}` expects a value");
                return ExitCode::from(2);
            }
        }
    }
    match velt_doc::site::build_site(&pages, &std_dir, &out) {
        Ok(n) => {
            eprintln!(
                "velt-site: wrote {n} pages to {}",
                out.join("index.html").display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
