//! A url dependency through the `velt` binary: installed from a local server, then a file of it
//! edited in place in the cache. A no-op build looks only at the tree's directories; whatever
//! loads the program again (`velt check`, a build that compiles) checks every file first.

use std::io::{Read, Write};
use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .output()
        .expect("run velt")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A `.tar.gz` of `files` (path, text).
fn tar_gz(files: &[(&str, &str)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(vec![]);
    for (path, text) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, path, text.as_bytes())
            .expect("tar");
    }
    let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gz.write_all(&tar.into_inner().expect("tar")).expect("gz");
    gz.finish().expect("gz")
}

/// Serve `body` to every request on this machine, from a thread; its URL.
fn serve(body: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}/lib.tar.gz")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn velt_add_warns_of_a_credential_in_the_url_and_later_commands_do_not() {
    let tmp = test_dir::TestDir::new();
    let (app, home) = (tmp.path().join("app"), tmp.path().join("home"));
    let archive = tar_gz(&[
        (
            "lib/package.velt",
            "export const pkg: Package = { name: \"lib\", version: \"1.0.0\" };\n",
        ),
        ("lib/src/lib.velt", "export const v = 1;\n"),
    ]);
    let url = format!("{}?token=abc", serve(archive));
    std::fs::create_dir_all(app.join("src")).expect("app");
    std::fs::write(
        app.join("package.velt"),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };\n",
    )
    .expect("manifest");
    std::fs::write(app.join("src/main.velt"), "console.log(1);\n").expect("main");
    let o = velt(&app, &home, &["add", &url]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(out.contains("looks like a credential"), "{out}");
    for command in ["check", "install"] {
        let o = velt(&app, &home, &[command]);
        let out = text(&o);
        assert!(o.status.success(), "{out}");
        assert!(!out.contains("looks like a credential"), "{command}: {out}");
    }
}

#[test]
fn velt_check_sees_a_fetched_file_edited_in_place() {
    let tmp = test_dir::TestDir::new();
    let (app, home) = (tmp.path().join("app"), tmp.path().join("home"));
    let archive = tar_gz(&[
        (
            "lib/package.velt",
            "export const pkg: Package = { name: \"lib\", version: \"1.0.0\" };\n",
        ),
        ("lib/src/lib.velt", "export const v = 1;\n"),
    ]);
    let url = serve(archive.clone());
    std::fs::create_dir_all(app.join("src")).expect("app");
    std::fs::write(
        app.join("package.velt"),
        format!(
            "export const pkg: Package = {{ name: \"app\", version: \"0.1.0\", dependencies: {{ lib: {{ url: \"{url}\", sha256: \"{}\", strip: 1 }} }} }};\n",
            sha256_hex(&archive)
        ),
    )
    .expect("manifest");
    std::fs::write(
        app.join("src/main.velt"),
        "import { v } from \"lib\";\n\nconsole.log(v);\n",
    )
    .expect("main");
    let o = velt(&app, &home, &["install"]);
    assert!(o.status.success(), "{}", text(&o));
    // A tree's stamp is written only once the tree is 3 s old (`vpm::tree_stamp`'s rule for times
    // too close to now), and only a stamp lets opening the project skip the files, which is the
    // case this test is about: check until there is one.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let o = velt(&app, &home, &["check"]);
        assert!(o.status.success(), "{}", text(&o));
        let stamps = std::fs::read_dir(home.join("cache/stamps")).map_or(0, Iterator::count);
        if stamps > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no tree stamp was written"
        );
    }
    // An edit in place, with the file's size kept: its directory's time doesn't change, so
    // opening the project trusts the stamp, and `velt check`'s own check must see the file.
    let cached = home
        .join("cache/url")
        .join(sha256_hex(&archive))
        .join("lib/src/lib.velt");
    std::fs::write(&cached, "export const v = 7;\n").expect("edit");
    let o = velt(&app, &home, &["check"]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("changed since it was fetched"), "{out}");
}
