//! `velt-launcher`, installed as `<root>/bin/velt` (see the crate's lib).

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = velt_launcher::main(&args);
    std::process::exit(code);
}
