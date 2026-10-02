//! The server end to end with vpm's client: publish over HTTP, resolve and install a dependent
//! package from the remote registry, and the upload checks.

use std::net::TcpListener;
use std::path::Path;

use velt_http::Server;
use vpm::{archive, InstallOptions, Locations};

use super::*;

/// A package whose manifest has `fields` after its name and version.
fn package(dir: &Path, name: &str, version: &str, fields: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = format!(
        "export const pkg: Package = {{ name: \"{name}\", version: \"{version}\", {fields} }};\n"
    );
    std::fs::write(dir.join("package.vlt"), manifest).unwrap();
    std::fs::write(
        dir.join("src/lib.vlt"),
        format!("export const V: i64 = 1; // {version}\n"),
    )
    .unwrap();
}

/// Serve `root`; with `user`, the registry has that user and the user's token is returned.
fn start(root: &Path, user: Option<&str>) -> (Server, String) {
    let token = user.map_or(String::new(), |u| auth::add_user(root, u).unwrap());
    let registry = Registry {
        root: root.to_path_buf(),
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = Server::start(listener, registry.handler(), MAX_ARCHIVE).unwrap();
    (server, token)
}

fn client(home: &Path, url: &str) -> Locations {
    let mut loc = Locations::under(home);
    loc.remote = Some(url.to_string());
    loc
}

#[test]
fn publish_and_install_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, _) = start(&tmp.path().join("server"), None);
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

    // An app on another "machine" (its own home/cache) names the registry in package.vlt.
    let app = tmp.path().join("app");
    let fields = format!("registry: \"{url}\", dependencies: {{ lib: \"1\" }}");
    package(&app, "app", "0.1.0", &fields);
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
    let (server, token) = start(&tmp.path().join("server"), Some("alice"));
    let url = format!("http://{}", server.addr());
    let pkg = tmp.path().join("p");
    package(&pkg, "p", "1.0.0", "");
    let body = archive::pack(&pkg).unwrap();
    let sum = archive::checksum(&body).unwrap();
    let put = |path: &str, headers: &[(&str, &str)]| {
        velt_http::fetch("PUT", &format!("{url}/api/v1/{path}"), headers, &body).unwrap()
    };
    let bearer = format!("Bearer {token}");
    let auth = ("Authorization", bearer.as_str());
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

fn native_bundle(dir: &Path, content: &str) -> std::path::PathBuf {
    let target = "x86_64-unknown-linux-gnu";
    let b = dir.join(target);
    std::fs::create_dir_all(b.join("shared")).unwrap();
    std::fs::write(b.join("shared/libvelt_native_n.so"), content).unwrap();
    let meta = vpm::native::NativeMeta {
        package: "n".into(),
        version: "1.0.0".into(),
        target: target.into(),
        abi: 1,
        shared: "shared/libvelt_native_n.so".into(),
        import_lib: None,
        static_obj: None,
        exports: Default::default(),
    };
    std::fs::write(b.join("native.json"), meta.to_json()).unwrap();
    b
}

#[test]
fn native_libraries_over_http() {
    let target = "x86_64-unknown-linux-gnu";
    let tmp = tempfile::tempdir().unwrap();
    let (server, token) = start(&tmp.path().join("server"), Some("alice"));
    let url = format!("http://{}", server.addr());
    let loc = client(&tmp.path().join("home"), &url);

    let lib = tmp.path().join("n");
    let fields = format!("native: {{ targets: [\"{target}\"] }}");
    package(&lib, "n", "1.0.0", &fields);
    std::fs::create_dir_all(lib.join("native")).unwrap();
    std::fs::write(lib.join("native/Cargo.toml"), "").unwrap();
    std::fs::write(lib.join("native/Cargo.lock"), "").unwrap();
    let bundle = native_bundle(&tmp.path().join("out"), "v1");
    let bundles = std::collections::BTreeMap::from([(target.to_string(), bundle.clone())]);
    std::env::set_var(vpm::remote::TOKEN_VAR, &token);
    let entry = vpm::registry::publish_with_native(&lib, &loc, &bundles).unwrap();
    assert_eq!(entry.native.len(), 1);

    // Another machine installs the prebuilt library.
    let app = tmp.path().join("app");
    package(&app, "app", "0.1.0", "dependencies: { n: \"1\" }");
    let other = client(&tmp.path().join("home2"), &url);
    let opts = InstallOptions {
        target: Some(target.into()),
        ..Default::default()
    };
    let installed = vpm::install(&app, &other, opts).unwrap();
    let (_, native) = installed.graph.natives().next().unwrap();
    assert_eq!(std::fs::read_to_string(native.shared_lib()).unwrap(), "v1");
    assert_eq!(installed.lockfile.get("n").unwrap().native, entry.native);

    // Uploads are checked: checksum, and a published target is never replaced.
    let path = format!("{url}/api/v1/n/1.0.0/native/{target}");
    let changed = native_bundle(&tmp.path().join("changed"), "v2");
    let body = vpm::native::bundle::pack(&changed).unwrap();
    let sum = vpm::native::bundle::checksum(&changed).unwrap();
    let bearer = format!("Bearer {token}");
    let auth = ("Authorization", bearer.as_str());
    let put = |headers: &[(&str, &str)]| velt_http::fetch("PUT", &path, headers, &body).unwrap();
    assert_eq!(put(&[auth, ("X-Velt-Checksum", "sha256:0")]).status, 400);
    assert_eq!(put(&[("X-Velt-Checksum", &sum)]).status, 401);
    assert_eq!(put(&[auth, ("X-Velt-Checksum", &sum)]).status, 409);
    let other_version = format!("{url}/api/v1/n/2.0.0/native/{target}");
    let resp = velt_http::fetch(
        "PUT",
        &other_version,
        &[auth, ("X-Velt-Checksum", &sum)],
        &body,
    );
    assert_eq!(resp.unwrap().status, 400); // the bundle names 1.0.0
    let unknown = format!("{url}/api/v1/n/1.0.0/native/sparc-sun-solaris");
    assert_eq!(
        velt_http::fetch("GET", &unknown, &[], b"").unwrap().status,
        400
    );
    server.stop();
}

/// A request to the server at `url` as the user with `token` (`""`: no token).
fn call(method: &str, url: &str, path: &str, token: &str) -> velt_http::Response {
    let bearer = format!("Bearer {token}");
    let headers: Vec<(&str, &str)> = match token {
        "" => vec![],
        _ => vec![("Authorization", bearer.as_str())],
    };
    velt_http::fetch(method, &format!("{url}/api/v1/{path}"), &headers, b"").unwrap()
}

/// Upload the package at `dir` as the user with `token` (without `$VELT_REGISTRY_TOKEN`, which
/// tests running in parallel would share).
fn upload(url: &str, dir: &Path, token: &str) -> velt_http::Response {
    let body = archive::pack(dir).unwrap();
    let sum = archive::checksum(&body).unwrap();
    let bearer = format!("Bearer {token}");
    let headers = [
        ("X-Velt-Checksum", sum.as_str()),
        ("Authorization", &bearer),
    ];
    let m = vpm::Manifest::from_dir(dir).unwrap();
    let target = format!("{url}/api/v1/{}/{}", m.package.name, m.package.version);
    velt_http::fetch("PUT", &target, &headers, &body).unwrap()
}

#[test]
fn owners_yank_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    let (server, alice) = start(&root, Some("alice"));
    let bob = auth::add_user(&root, "bob").unwrap();
    let url = format!("http://{}", server.addr());
    let loc = client(&tmp.path().join("home"), &url);

    // alice publishes `json` and owns it; bob can't publish it.
    let lib = tmp.path().join("json");
    package(&lib, "json", "1.0.0", "");
    let publish = |token: &str| upload(&url, &lib, token);
    assert_eq!(publish(&alice).status, 201);
    assert_eq!(call("GET", &url, "json/owners", "").body_text(), "alice\n");
    package(&lib, "json", "1.1.0", "");
    let denied = publish(&bob);
    assert_eq!(denied.status, 403, "{}", denied.body_text());
    assert!(denied
        .body_text()
        .contains("`bob` is not an owner of `json`"));

    // Owners: only owners change them; users must exist; the last one stays.
    assert_eq!(call("PUT", &url, "json/owners/bob", &bob).status, 403);
    assert_eq!(call("PUT", &url, "json/owners/carol", &alice).status, 404);
    assert_eq!(call("PUT", &url, "json/owners/bob", &alice).status, 200);
    assert_eq!(publish(&bob).status, 201);
    assert_eq!(call("DELETE", &url, "json/owners/alice", &bob).status, 200);
    assert_eq!(call("DELETE", &url, "json/owners/bob", &bob).status, 409);
    assert_eq!(call("GET", &url, "json/owners", "").body_text(), "bob\n");
    assert_eq!(call("GET", &url, "nope/owners", "").status, 404);

    // Yank: owners only; the index records it and search skips it.
    assert_eq!(call("PUT", &url, "json/1.1.0/yank", &alice).status, 403);
    assert_eq!(call("PUT", &url, "json/1.1.0/yank", "").status, 401);
    assert_eq!(call("PUT", &url, "json/1.1.0/yank", &bob).status, 200);
    assert_eq!(call("PUT", &url, "json/9.9.9/yank", &bob).status, 404);
    let index = vpm::registry::read_index(&loc, "json").unwrap().unwrap();
    assert_eq!(
        index
            .versions
            .iter()
            .map(|v| (v.version.as_str(), v.yanked))
            .collect::<Vec<_>>(),
        [("1.0.0", false), ("1.1.0", true)]
    );
    let hits = vpm::search::search(&loc, "JS").unwrap();
    assert_eq!(
        hits,
        [vpm::search::Hit {
            name: "json".into(),
            version: "1.0.0".into()
        }]
    );
    assert!(vpm::search::search(&loc, "http").unwrap().is_empty());
    assert_eq!(call("DELETE", &url, "json/1.1.0/yank", &bob).status, 200);
    assert_eq!(
        vpm::search::search(&loc, "json").unwrap()[0].version,
        "1.1.0"
    );
    server.stop();
}

#[test]
fn an_open_registry_has_no_owners() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("server");
    let (server, _) = start(&root, None);
    let url = format!("http://{}", server.addr());
    let loc = client(&tmp.path().join("home"), &url);
    let lib = tmp.path().join("lib");
    package(&lib, "lib", "1.0.0", "");
    vpm::registry::publish(&lib, &loc).unwrap();
    vpm::yank::yank(&loc, "lib", "1.0.0", true).unwrap();
    assert_eq!(call("GET", &url, "lib/owners", "").body_text(), "");
    let refused = call("PUT", &url, "lib/owners/alice", "");
    assert_eq!(refused.status, 400);
    assert!(refused.body_text().contains("has no users"));

    // Once the registry has users, nobody may take over the unowned package...
    let alice = auth::add_user(&root, "alice").unwrap();
    assert_eq!(call("DELETE", &url, "lib/1.0.0/yank", "").status, 401);
    let taken = call("PUT", &url, "lib/owners/alice", &alice);
    assert_eq!(taken.status, 403);
    assert!(
        taken.body_text().contains("velt registry owner add"),
        "{}",
        taken.body_text()
    );
    assert_eq!(call("DELETE", &url, "lib/1.0.0/yank", &alice).status, 403);
    package(&lib, "lib", "1.1.0", "");
    assert_eq!(upload(&url, &lib, &alice).status, 403);
    // ...until an administrator assigns an owner; a new package is still claimed by its first
    // publisher.
    owners::set_by_admin(&root, "lib", "alice", true).unwrap();
    assert_eq!(call("DELETE", &url, "lib/1.0.0/yank", &alice).status, 200);
    assert_eq!(upload(&url, &lib, &alice).status, 201);
    let fresh = tmp.path().join("fresh");
    package(&fresh, "fresh", "1.0.0", "");
    assert_eq!(upload(&url, &fresh, &alice).status, 201);
    assert_eq!(call("GET", &url, "fresh/owners", "").body_text(), "alice\n");
    // The administrator may remove the last owner (and assign another).
    assert!(owners::set_by_admin(&root, "lib", "alice", false)
        .unwrap()
        .is_empty());
    assert!(owners::set_by_admin(&root, "lib", "nobody", true).is_err());
    assert!(owners::set_by_admin(&root, "nope", "alice", true).is_err());
    assert!(owners::set_by_admin(&root, "..", "alice", true).is_err());
    server.stop();
}

mod access;
mod uploads;
