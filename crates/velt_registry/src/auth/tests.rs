use super::*;

fn request(auth: Option<&str>) -> Request {
    let mut req = Request {
        method: "PUT".into(),
        ..Default::default()
    };
    if let Some(value) = auth {
        req.headers.push(("authorization".into(), value.into()));
    }
    req
}

fn bearer(token: &str) -> Option<String> {
    Some(format!("Bearer {token}"))
}

#[test]
fn users_tokens_and_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert_eq!(caller(root, &request(None)), Ok(Caller::Anyone));
    assert!(is_open(root).unwrap());

    let alice = add_user(root, "alice").unwrap();
    assert_eq!(alice.len(), 64);
    assert!(!is_open(root).unwrap());
    let bob = add_user(root, "bob").unwrap();
    assert_ne!(alice, bob);
    let stored = std::fs::read_to_string(root.join(USERS_FILE)).unwrap();
    assert!(!stored.contains(&alice), "tokens are stored hashed");
    assert!(add_user(root, "alice")
        .unwrap_err()
        .contains("already exists"));
    assert!(add_user(root, "Bad Name").is_err());

    assert_eq!(
        caller(root, &request(bearer(&alice).as_deref())),
        Ok(Caller::User("alice".into()))
    );
    // The scheme is case-insensitive (RFC 7235).
    let lower = format!("bearer {alice}");
    assert_eq!(
        caller(root, &request(Some(&lower))),
        Ok(Caller::User("alice".into()))
    );
    for bad in [
        None,
        Some(""),
        Some("Bearer "),
        Some("Bearer nope"),
        Some(alice.as_str()),
    ] {
        assert_eq!(caller(root, &request(bad)).unwrap_err().status, 401);
    }

    let alice2 = rotate_token(root, "alice").unwrap();
    let old = bearer(&alice);
    assert_eq!(
        caller(root, &request(old.as_deref())).unwrap_err().status,
        401
    );
    assert_eq!(
        caller(root, &request(bearer(&alice2).as_deref())),
        Ok(Caller::User("alice".into()))
    );
    remove_user(root, "bob", false).unwrap();
    let gone = bearer(&bob);
    assert_eq!(
        caller(root, &request(gone.as_deref())).unwrap_err().status,
        401
    );
    assert_eq!(user_names(root).unwrap(), ["alice"]);
    assert!(remove_user(root, "bob", false).is_err());
}

#[test]
fn removing_the_last_user_needs_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    add_user(root, "alice").unwrap();
    let err = remove_user(root, "alice", false).unwrap_err();
    assert!(err.contains("--open"), "{err}");
    assert!(!is_open(root).unwrap());
    remove_user(root, "alice", true).unwrap();
    assert!(is_open(root).unwrap());
    assert_eq!(caller(root, &request(None)), Ok(Caller::Anyone));
}

#[test]
fn an_empty_or_damaged_users_file_refuses_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".auth")).unwrap();
    std::fs::write(root.join(USERS_FILE), "{ \"users\": {} }").unwrap();
    assert_eq!(caller(root, &request(None)).unwrap_err().status, 401);
    for damaged in ["", "{\"users\": ["] {
        std::fs::write(root.join(USERS_FILE), damaged).unwrap();
        assert_eq!(caller(root, &request(None)).unwrap_err().status, 500);
    }
}

#[test]
fn the_users_file_is_pretty_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    add_user(root, "alice").unwrap();
    let text = std::fs::read_to_string(root.join(USERS_FILE)).unwrap();
    assert!(
        text.starts_with("{\n  \"users\": {\n    \"alice\": \"sha256:"),
        "{text}"
    );
    assert!(text.ends_with("}\n"), "{text}");
}

#[test]
fn the_users_file_is_replaced_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    add_user(root, "alice").unwrap();
    add_user(root, "bob").unwrap();
    let names: Vec<String> = std::fs::read_dir(root.join(".auth"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["users.json"], "no temporary file is left behind");
}
