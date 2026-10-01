//! The reload channel (hot swap, docs/internals/design/hot-reload.md phase 3): after `go`, a JIT host keeps
//! its build-report connection open, and the supervisor asks it to take each later version
//! there:
//! - supervisor → host: `reload\n`;
//! - host → supervisor: `swapped <n>\n` (n functions swapped in), `restart <reason>\n` (the
//!   program must restart to run the new version), or `failed\n` (the new version did not
//!   build); then one line per source file the build read, and an empty line.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use super::{expect_line, Stream, MAX_REPORT};

/// What a host did with a new version.
#[derive(Debug, PartialEq, Eq)]
pub enum Reloaded {
    /// The changed functions were swapped into the running program.
    Swapped {
        /// How many functions were swapped in.
        functions: usize,
        /// Every source file the build read.
        files: Vec<PathBuf>,
    },
    /// The program must restart to run the new version; the old one keeps running until then.
    Restart {
        /// Why, for the user.
        reason: String,
        /// Every source file the build read.
        files: Vec<PathBuf>,
    },
    /// The new version did not build (the host printed the diagnostics).
    Failed {
        /// Every source file the build read.
        files: Vec<PathBuf>,
    },
}

/// Host side: block until the supervisor asks for a reload (an error once it has gone away).
pub fn wait_reload(stream: &Stream) -> io::Result<()> {
    expect_line(stream, "reload")
}

/// Host side: answer a reload request.
pub fn reply_reload(stream: &Stream, reply: &Reloaded) -> io::Result<()> {
    let (head, files) = match reply {
        Reloaded::Swapped { functions, files } => (format!("swapped {functions}"), files),
        Reloaded::Restart { reason, files } => {
            (format!("restart {}", reason.replace('\n', " ")), files)
        }
        Reloaded::Failed { files } => ("failed".to_string(), files),
    };
    let mut text = format!("{head}\n");
    super::push_files(&mut text, files);
    (&*stream).write_all(text.as_bytes())
}

/// Supervisor side: ask the host on `stream` to take the current sources; blocks until it has.
pub fn request_reload(stream: &Stream) -> io::Result<Reloaded> {
    (&*stream).write_all(b"reload\n")?;
    let mut reader = BufReader::new(stream.take(MAX_REPORT));
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    let head = line.trim_end().to_string();
    let files = super::read_files(&mut reader)?;
    if let Some(n) = head.strip_prefix("swapped ") {
        let functions = n
            .parse()
            .map_err(|_| io::Error::other(format!("bad reload reply `{head}`")))?;
        return Ok(Reloaded::Swapped { functions, files });
    }
    if let Some(reason) = head.strip_prefix("restart ") {
        let reason = reason.to_string();
        return Ok(Reloaded::Restart { reason, files });
    }
    match head.as_str() {
        "failed" => Ok(Reloaded::Failed { files }),
        other => Err(io::Error::other(format!("bad reload reply `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{report_build, Server};
    use super::*;

    #[test]
    fn reload_round_trip() {
        let mut server = Server::bind().unwrap();
        let name = server.name().to_os_string();
        let files = vec![PathBuf::from("/a/main.vlt")];
        let sent = files.clone();
        let host = std::thread::spawn(move || {
            let stream = report_build(&name, true, &sent).unwrap().unwrap();
            let replies = [
                Reloaded::Swapped {
                    functions: 3,
                    files: sent.clone(),
                },
                Reloaded::Restart {
                    reason: "Point gained\na field".into(),
                    files: vec![],
                },
                Reloaded::Failed { files: sent },
            ];
            for reply in &replies {
                wait_reload(&stream).unwrap();
                reply_reload(&stream, reply).unwrap();
            }
            // The supervisor hangs up: the host's wait ends with an error.
            assert!(wait_reload(&stream).is_err());
        });
        let s = server.accept().unwrap();
        super::super::read_request(&s).unwrap();
        super::super::reply_go(&s).unwrap();
        let swapped = Reloaded::Swapped {
            functions: 3,
            files: files.clone(),
        };
        assert_eq!(request_reload(&s).unwrap(), swapped);
        let restart = Reloaded::Restart {
            reason: "Point gained a field".into(),
            files: vec![],
        };
        assert_eq!(request_reload(&s).unwrap(), restart);
        assert_eq!(request_reload(&s).unwrap(), Reloaded::Failed { files });
        drop(s);
        host.join().unwrap();
    }
}
