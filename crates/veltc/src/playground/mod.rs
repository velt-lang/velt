//! `velt playground`: a local web page to write and run Velt programs in the browser.
//!
//! The compiler cannot run in the browser (Cranelift/LLVM and the linker are native), so the
//! playground compiles on the server: the page posts the program to `/api/compile`, gets a
//! `wasm32-unknown-unknown` module back (or the diagnostics), and runs it in a Web Worker with
//! the same JS glue `velt build --target wasm32-unknown-unknown` writes (`velt_web.mjs`). The
//! program runs in the visitor's browser, never on the server.
//!
//! Routes: `GET /` (page), `/playground.js`, `/worker.js`, `/velt_web.mjs`, `/style.css`;
//! `POST /api/compile[?release=1]` (body: source) → `200 application/wasm` or `422` text.

mod compile;

use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::Arc;

use velt_http::{Handler, Request, Response, Server};

pub use compile::compile;

const INDEX: &str = include_str!("../../../../playground/index.html");
const SCRIPT: &str = include_str!("../../../../playground/playground.js");
const WORKER: &str = include_str!("../../../../playground/worker.js");
const STYLE: &str = include_str!("../../../../playground/style.css");
const GLUE: &str = include_str!("../../../velt_rt_wasm/js/velt_web.mjs");

/// `velt playground` options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaygroundArgs {
    /// Address to listen on (`127.0.0.1:8090` by default; port 0 = any free port).
    pub addr: String,
}

/// Answer one request.
pub fn route(req: Request) -> Response {
    let js = "text/javascript; charset=utf-8";
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/" | "/index.html") => {
            Response::bytes(200, "text/html; charset=utf-8", INDEX.into())
        }
        ("GET", "/playground.js") => Response::bytes(200, js, SCRIPT.into()),
        ("GET", "/worker.js") => Response::bytes(200, js, WORKER.into()),
        ("GET", "/velt_web.mjs") => Response::bytes(200, js, GLUE.into()),
        ("GET", "/style.css") => Response::bytes(200, "text/css; charset=utf-8", STYLE.into()),
        ("POST", "/api/compile") => compile_request(&req),
        (_, "/api/compile") => Response::text(405, "use POST"),
        _ => Response::text(404, "not found"),
    }
}

fn compile_request(req: &Request) -> Response {
    let Ok(source) = std::str::from_utf8(&req.body) else {
        return Response::text(400, "the program must be UTF-8 text");
    };
    let release = req.query.split('&').any(|p| p == "release=1");
    match compile(source, release) {
        Ok(module) => Response::bytes(200, "application/wasm", module),
        Err(diagnostics) => Response::text(422, diagnostics),
    }
}

/// Start the playground on `listener` in the background.
pub fn start(listener: TcpListener) -> std::io::Result<Server> {
    let handler: Handler = Arc::new(route);
    Server::start(listener, handler, compile::MAX_SOURCE)
}

/// `velt playground`: serve until interrupted.
pub fn playground_command(args: &PlaygroundArgs) -> ExitCode {
    if let Err(msg) = velt_link::find_runtime_lib(compile::TARGET) {
        crate::style::error(&msg);
        return ExitCode::from(1);
    }
    let listener = match TcpListener::bind(&args.addr) {
        Ok(l) => l,
        Err(e) => {
            crate::style::error(&format!("cannot listen on {}: {e}", args.addr));
            return ExitCode::from(1);
        }
    };
    match listener.local_addr() {
        Ok(addr) => eprintln!("velt playground: http://{addr}/ (Ctrl+C to stop)"),
        Err(e) => eprintln!("velt playground: listening ({e})"),
    }
    velt_http::serve(&listener, Arc::new(route), compile::MAX_SOURCE);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(path: &str) -> Response {
        route(Request {
            method: "GET".into(),
            path: path.into(),
            ..Default::default()
        })
    }

    #[test]
    fn static_assets() {
        assert!(get("/")
            .body_text()
            .contains("<title>Velt Playground</title>"));
        assert_eq!(get("/worker.js").status, 200);
        assert!(get("/velt_web.mjs").body_text().contains("runVelt"));
        assert_eq!(get("/nope").status, 404);
        assert_eq!(get("/api/compile").status, 405);
    }

    #[test]
    fn compile_errors_are_422() {
        let resp = route(Request {
            method: "POST".into(),
            path: "/api/compile".into(),
            body: b"function main() { let x: i64 = \"s\"; }".to_vec(),
            ..Default::default()
        });
        assert_eq!(resp.status, 422, "{}", resp.body_text());
        assert!(
            resp.body_text().contains("main.vlt"),
            "{}",
            resp.body_text()
        );
    }
}
