//! Uploads whose manifest is wrong: read as data, with the limits of every manifest, and
//! reported as `package.velt` (never the server's staging directory).

use super::*;

/// Uploads a hand-made archive of `files` as `p` 1.0.0 and returns the answer.
fn upload_files(url: &str, files: &[(&str, &[u8])]) -> velt_http::Response {
    let mut body = b"VELTPKG1\n".to_vec();
    for (path, bytes) in files {
        body.extend_from_slice(format!("{path}\n{}\n", bytes.len()).as_bytes());
        body.extend_from_slice(bytes);
    }
    let entries = files
        .iter()
        .map(|(path, bytes)| archive::Entry {
            path: path.to_string(),
            bytes: bytes.to_vec(),
        })
        .collect();
    let sum = archive::checksum_of(entries).unwrap();
    let headers = [("X-Velt-Checksum", sum.as_str())];
    velt_http::fetch("PUT", &format!("{url}/api/v1/p/1.0.0"), &headers, &body).unwrap()
}

#[test]
fn uploaded_manifests_are_read_as_data_and_named_package_velt() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, _) = start(&tmp.path().join("server"), None);
    let url = format!("http://{}", server.addr());
    let rejected = |files: &[(&str, &[u8])], message: &str| {
        let answer = upload_files(&url, files);
        let body = answer.body_text();
        assert_eq!(answer.status, 400, "{body}");
        assert!(body.contains(message), "{body}");
        assert!(!body.contains(".staging"), "the staging path leaks: {body}");
    };
    rejected(
        &[("velt.toml", b"[package]\n"), ("src/lib.velt", b"")],
        "published with a `velt.toml`",
    );
    rejected(&[("src/lib.velt", b"")], "the archive has no package.velt");
    let code = b"import { readFile } from \"velt/fs\";\nexport const pkg: Package = { name: \"p\", version: \"1.0.0\" };\n";
    rejected(
        &[("package.velt", code)],
        "package.velt:1:1: error: `package.velt` may only import types from `velt/package`",
    );
    let huge = format!("// {}\n", "x".repeat(vpm::manifest::read::MAX_BYTES));
    rejected(
        &[("package.velt", huge.as_bytes())],
        "package.velt: the manifest is larger than 64 KiB",
    );
    rejected(
        &[("package.velt", b"// \xff\n")],
        "package.velt: the manifest is not valid UTF-8",
    );
    let ok = b"export const pkg: Package = { name: \"p\", version: \"1.0.0\" };\n";
    assert_eq!(upload_files(&url, &[("package.velt", ok)]).status, 201);
    assert!(!tmp
        .path()
        .join("server/.staging")
        .read_dir()
        .unwrap()
        .any(|_| true));
    server.stop();
}
