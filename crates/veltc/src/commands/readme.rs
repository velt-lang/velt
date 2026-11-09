//! `velt publish`'s README: the package's `README.md`, sent to a remote registry after the
//! version is published (`PUT /api/v1/<name>/<version>/readme`), for the registry's page. It is
//! not one of the package's files, so it changes no checksum.

use std::path::{Path, PathBuf};

use crate::style;

/// The package's README at `root` (`README.md`, in any case), if it has one.
fn readme_of(root: &Path) -> Option<PathBuf> {
    // Read from the directory, so the name is the file's own also where names ignore case;
    // `README.md` first when there are several.
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("readme.md"))
                && p.is_file()
        })
        .collect();
    found.sort_by_key(|p| p.file_name().is_none_or(|n| n != "README.md"));
    found.into_iter().next()
}

/// Sends the package's README to the registry at `url` for the version just published. The
/// version is published either way: a README that fails is a warning.
pub(super) fn send(root: &Path, url: &str, name: &str, version: &str) {
    let Some(path) = readme_of(root) else {
        return;
    };
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = match std::fs::read(&path).map(String::from_utf8) {
        Ok(Ok(text)) => text,
        Ok(Err(_)) => {
            return style::warning(&format!(
                "`{file}` is not UTF-8 text; the registry shows no README for `{name}` {version}"
            ))
        }
        Err(e) => return style::warning(&format!("cannot read `{}`: {e}", path.display())),
    };
    match vpm::remote::publish_readme(url, name, version, &text) {
        Ok(vpm::remote::ReadmeSent::Stored) => {
            style::status("README", &format!("`{file}` for the registry's page"))
        }
        Ok(vpm::remote::ReadmeSent::Unsupported) => {}
        Err(e) => style::warning(&format!(
            "`{name}` {version} is published, but its README is not: {e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use velt_http::{Request, Response, Server};

    #[test]
    fn the_readme_is_found_in_any_case_and_sent_as_text() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(super::readme_of(root), None);
        std::fs::write(root.join("Readme.md"), "# lib\n").unwrap();
        assert_eq!(super::readme_of(root), Some(root.join("Readme.md")));
        // A directory of that name isn't one.
        let other = tmp.path().join("other");
        std::fs::create_dir_all(other.join("README.md")).unwrap();
        assert_eq!(super::readme_of(&other), None);

        let got: Arc<Mutex<Vec<String>>> = Arc::default();
        let g = got.clone();
        let handler = Arc::new(move |req: Request| {
            let body = String::from_utf8_lossy(&req.body).into_owned();
            g.lock()
                .unwrap()
                .push(format!("{} {} {body}", req.method, req.path));
            Response::text(200, "stored")
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::start(listener, handler, 1 << 16).unwrap();
        let url = format!("http://{}", server.addr());
        super::send(root, &url, "lib", "1.0.0");
        // Not sent: none there, or not UTF-8.
        super::send(&other, &url, "lib", "1.0.0");
        std::fs::write(root.join("Readme.md"), [0xff, 0xfe]).unwrap();
        super::send(root, &url, "lib", "1.0.0");
        assert_eq!(
            *got.lock().unwrap(),
            ["PUT /api/v1/lib/1.0.0/readme # lib\n"]
        );
        server.stop();
    }
}
