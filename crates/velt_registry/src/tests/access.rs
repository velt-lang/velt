//! Who may do what: names that would leave the registry directory, writes without a valid
//! token, and writes by users who don't own the package.

use super::*;

/// A registry with users `alice` (owner of the published `lib`) and `bob`; returns the server,
/// its URL and both tokens.
fn registry_with_lib(tmp: &Path) -> (Server, String, String, String) {
    let root = tmp.join("server");
    let (server, alice) = start(&root, Some("alice"));
    let bob = auth::add_user(&root, "bob").unwrap();
    let url = format!("http://{}", server.addr());
    let lib = tmp.join("lib");
    package(&lib, "lib", "1.0.0", "");
    assert_eq!(upload(&url, &lib, &alice).status, 201);
    (server, url, alice, bob)
}

#[test]
fn names_never_leave_the_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, url, alice, _) = registry_with_lib(tmp.path());
    // A directory next to the registry that looks like a package.
    let outside = tmp.path().join("other");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join(vpm::registry::INDEX_FILE), "").unwrap();
    for name in ["..", "..%5Cother", "..\\other", ".auth", ".staging", "Lib"] {
        let get = call("GET", &url, &format!("{name}/owners"), "");
        assert!(
            matches!(get.status, 400 | 404),
            "GET {name}: {}",
            get.status
        );
        assert_ne!(get.status, 200, "GET {name}");
        let put = call("PUT", &url, &format!("{name}/owners/bob"), &alice);
        assert_eq!(put.status, 400, "PUT {name}: {}", put.body_text());
    }
    for user in ["..", ".auth", "Bob", "..%5Cx"] {
        let put = call("PUT", &url, &format!("lib/owners/{user}"), &alice);
        assert_eq!(put.status, 400, "PUT user {user}: {}", put.body_text());
    }
    assert!(!outside.join(owners::OWNERS_FILE).exists());
    assert!(!tmp
        .path()
        .join("server/.auth")
        .join(owners::OWNERS_FILE)
        .exists());
    server.stop();
}

#[test]
fn owner_changes_need_a_valid_token() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, url, alice, _) = registry_with_lib(tmp.path());
    for method in ["PUT", "DELETE"] {
        for token in ["", "not-a-token"] {
            let answer = call(method, &url, "lib/owners/bob", token);
            assert_eq!(answer.status, 401, "{method} with `{token}`");
        }
    }
    assert_eq!(call("DELETE", &url, "lib/owners/bob", &alice).status, 404);
    assert_eq!(call("GET", &url, "lib/owners", "").body_text(), "alice\n");
    server.stop();
}

#[test]
fn only_owners_add_native_libraries() {
    let target = "x86_64-unknown-linux-gnu";
    let tmp = tempfile::tempdir().unwrap();
    let (server, url, _, bob) = registry_with_lib(tmp.path());
    let bundle = native_bundle(&tmp.path().join("out"), "v1");
    let body = vpm::native::bundle::pack(&bundle).unwrap();
    let sum = vpm::native::bundle::checksum(&bundle).unwrap();
    let path = format!("{url}/api/v1/lib/1.0.0/native/{target}");
    let bearer = format!("Bearer {bob}");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("X-Velt-Checksum", &sum),
    ];
    let answer = velt_http::fetch("PUT", &path, &headers, &body).unwrap();
    assert_eq!(answer.status, 403, "{}", answer.body_text());
    assert!(answer
        .body_text()
        .contains("`bob` is not an owner of `lib`"));
    server.stop();
}
