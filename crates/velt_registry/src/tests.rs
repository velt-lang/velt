//! The server end to end with vpm's client: publish over HTTP, resolve and install a dependent
//! package from the remote registry, and the upload checks.

use std::net::TcpListener;
use std::path::Path;

use velt_http::Server;
use vpm::{InstallOptions, Locations};

use super::*;

fn package(dir: &Path, name: &str, version: &str, deps: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest =
        format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n[dependencies]\n{deps}");
    std::fs::write(dir.join("velt.toml"), manifest).unwrap();
    std::fs::write(
        dir.join("src/lib.vlt"),
        format!("export const V: i64 = 1; // {version}\n"),
    )
    .unwrap();
}

fn start(root: &Path, token: Option<&str>) -> Server {
    let registry = Registry {
        root: root.to_path_buf(),
        token: token.map(String::from),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    Server::start(listener, registry.handler(), MAX_ARCHIVE).unwrap()
}

fn client(home: &Path, url: &str) -> Locations {
    let mut loc = Locations::under(home);
    loc.remote = Some(url.to_string());
    loc
}

#[test]
fn publish_and_install_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(&tmp.path().join("server"), None);
    let url = format!("http://{}", server.addr());
    let loc = client(&tmp.path().join("home"), &url);

    let lib = tmp.path().join("lib");
    package(&lib, "lib", "1.0.0", "");
    let entry = vpm::registry::publish(&lib, &loc).unwrap();
    assert!(vpm::registry::publish(&lib, &loc)
        .unwrap_err()
        .contains("already published"));
    package(&lib, "lib", "1.1.0", "");
    vpm::registry::publish(&lib, &loc).unwrap();
    let index = vpm::registry::read_index(&loc, "lib").unwrap().unwrap();
    assert_eq!(index.versions.len(), 2);
    assert_eq!(index.versions[0], entry);
    assert_eq!(vpm::registry::read_index(&loc, "nope").unwrap(), None);

    // An app on another "machine" (its own home/cache) names the registry in velt.toml.
    let app = tmp.path().join("app");
    package(&app, "app", "0.1.0", "lib = \"1\"\n");
    let manifest = std::fs::read_to_string(app.join("velt.toml")).unwrap();
    std::fs::write(
        app.join("velt.toml"),
        format!("registry = \"{url}\"\n{manifest}"),
    )
    .unwrap();
    let other = Locations::under(&tmp.path().join("home2"));
    let installed = vpm::install(&app, &other, InstallOptions::default()).unwrap();
    let locked = installed.lockfile.get("lib").unwrap();
    assert_eq!(locked.version, "1.1.0");
    let cached = tmp.path().join("home2/cache/lib-1.1.0/src/lib.vlt");
    assert!(std::fs::read_to_string(cached).unwrap().contains("1.1.0"));
    let listing = velt_http::fetch("GET", &format!("{url}/"), &[], b"").unwrap();
    assert!(
        listing.body_text().contains("lib"),
        "{}",
        listing.body_text()
    );
    server.stop();
}

#[test]
fn uploads_are_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(&tmp.path().join("server"), Some("s3cret"));
    let url = format!("http://{}", server.addr());
    let pkg = tmp.path().join("p");
    package(&pkg, "p", "1.0.0", "");
    let body = archive::pack(&pkg).unwrap();
    let sum = archive::checksum(&body).unwrap();
    let put = |path: &str, headers: &[(&str, &str)]| {
        velt_http::fetch("PUT", &format!("{url}/api/v1/{path}"), headers, &body).unwrap()
    };
    let auth = ("Authorization", "Bearer s3cret");
    assert_eq!(put("p/1.0.0", &[("X-Velt-Checksum", &sum)]).status, 401);
    assert_eq!(
        put("p/1.0.0", &[auth, ("X-Velt-Checksum", "sha256:0")]).status,
        400
    );
    let wrong = put("q/1.0.0", &[auth, ("X-Velt-Checksum", &sum)]);
    assert!(
        wrong.body_text().contains("not `q` 1.0.0"),
        "{}",
        wrong.body_text()
    );
    assert_eq!(
        put("p/1.0.0", &[auth, ("X-Velt-Checksum", &sum)]).status,
        201
    );
    assert_eq!(
        put("p/1.0.0", &[auth, ("X-Velt-Checksum", &sum)]).status,
        409
    );
    assert_eq!(
        put("../x/1.0.0", &[auth, ("X-Velt-Checksum", &sum)]).status,
        404
    );
    let got = velt_http::fetch("GET", &format!("{url}/api/v1/p/1.0.0"), &[], b"").unwrap();
    assert_eq!(archive::checksum(&got.body).unwrap(), sum);
    assert!(!tmp
        .path()
        .join("server/.staging")
        .read_dir()
        .unwrap()
        .any(|_| true));
    server.stop();
}
