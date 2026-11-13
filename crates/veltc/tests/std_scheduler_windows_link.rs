//! The C functions a `VELT_SCHEDULER=std` program calls on Windows are ones the link's import
//! libraries export (std/lang/hooks/reactor_afd.velt, std/net/calls_win32.velt), checked from
//! any OS through the LLVM IR. Split from std_scheduler_net.rs.

use std::path::Path;

use no_window::command;

mod no_window;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Windows' reactor (std/lang/hooks/reactor_afd.velt) and its Winsock calls (std/net/
/// calls_win32.velt), the threads and the parking: every C function a `VELT_SCHEDULER=std`
/// program using sockets, name lookups and files calls on Windows is one the link's import
/// libraries export (kernel32, ntdll, ws2_32, the synchronization library, the UCRT), debug and
/// release. Checked from any OS through the LLVM IR, since a Windows link needs Windows; the
/// POSIX functions the executor called (`clock_gettime`, `sched_yield`, `close`) exist in none.
#[test]
fn a_windows_program_calls_only_functions_its_import_libraries_export() {
    let kit = manifest().join("../velt_link/kit/windows");
    let mut exported = std::collections::HashSet::new();
    for lib in ["kernel32", "ntdll", "ws2_32", "synchronization", "ucrt"] {
        let def = std::fs::read_to_string(kit.join(format!("{lib}.def"))).unwrap();
        for line in def
            .lines()
            .skip_while(|l| !l.starts_with("EXPORTS"))
            .skip(1)
        {
            if let Some(name) = line.split_whitespace().next() {
                exported.insert(name.to_string());
            }
        }
    }
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("main.velt"),
        "import { connect, listen } from \"velt/net\";
import { lookup } from \"velt/net/dns\";
import { readFile, writeFile } from \"velt/fs\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const s = await l.accept();
    s.writeBytesLater(await s.read());
    console.log(s.peerAddr());
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write(\"hi\");
  console.log(await c.readString(), await c.readableWithin(10));
  c.shutdown();
  c.close();
  await server;
  l.close();
  console.log((await lookup(\"localhost\")).length);
  await writeFile(\"x.txt\", \"y\");
  console.log(await readFile(\"x.txt\"));
}
",
    )
    .unwrap();
    for release in [false, true] {
        let mut c = command(env!("CARGO_BIN_EXE_velt"));
        c.args(["build", "main.velt", "--emit", "llvm"])
            .args(["--target", "x86_64-pc-windows-msvc"])
            .current_dir(dir.path())
            .env("VELT_SCHEDULER", "std");
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let ir = String::from_utf8_lossy(&o.stdout);
        assert!(
            ir.contains("NtDeviceIoControlFile"),
            "no Windows reactor (release: {release})"
        );
        let missing: Vec<&str> = ir
            .lines()
            .filter(|l| l.starts_with("declare "))
            .filter_map(|l| l.split('@').nth(1)?.split('(').next())
            .map(|n| n.trim_matches('"'))
            .filter(|n| !n.starts_with("velt_") && !n.starts_with("llvm."))
            .filter(|n| !exported.contains(*n))
            .collect();
        assert!(missing.is_empty(), "release: {release}: {missing:?}");
    }
}
