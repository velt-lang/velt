//! A stand-in toolchain `bin/velt` for the launcher's tests (tests/side_by_side.rs packs it into
//! fake releases): it prints which prefix it is, what the launcher told it, and its arguments.

fn main() {
    let exe = std::env::current_exe().unwrap();
    let prefix = exe.parent().and_then(|bin| bin.parent()).unwrap();
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    println!("prefix={}", prefix.display());
    println!("selected={}", var("VELT_TOOLCHAIN_SELECTED"));
    println!("launcher={}", var("VELT_LAUNCHER"));
    println!("args={}", args.join(" "));
    let code = args
        .iter()
        .find_map(|a| a.strip_prefix("--exit="))
        .map_or(0, |c| c.parse().unwrap());
    std::process::exit(code);
}
