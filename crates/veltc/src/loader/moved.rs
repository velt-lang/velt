//! Where the modules of the standard library's former layout went (docs/internals/design/
//! std-layout.md): for the error on a `velt:<old>` specifier and a note on `cannot find module
//! velt/<old>`, so a program written for the old layout is told the new import.

/// Where the std docs are published (an installed toolchain has no `docs/`).
pub const STD_DOCS_URL: &str = "https://veltlang.com/docs/std/";

/// Where an old module went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// To this module (`velt/encoding/json`).
    Module(&'static str),
    /// Its names are globals now: no import.
    Globals,
    /// Out of the standard library, into a package of the Velt repository (`packages/<name>`),
    /// imported by its name; the text says what it is (`SQLite`).
    Package(&'static str, &'static str),
    /// The hyper HTTP server: the `http-server` package serves, `velt/net/http` has `fetch`.
    HttpServer,
}

/// Old module (as written after `velt:`, or after `velt/` once) → where it went.
const MOVED: &[(&str, Moved)] = &[
    ("arena", Moved::Module("velt/mem")),
    ("bigint", Moved::Module("velt/math")),
    ("channel", Moved::Module("velt/sync")),
    ("child_process", Moved::Module("velt/process/child")),
    ("cli", Moved::Module("velt/cli")),
    ("collections/btree_map", Moved::Module("velt/collections")),
    (
        "collections/concurrent_map",
        Moved::Module("velt/collections"),
    ),
    ("collections/deque", Moved::Module("velt/collections")),
    (
        "collections/priority_queue",
        Moved::Module("velt/collections"),
    ),
    ("collections/set", Moved::Module("velt/collections")),
    ("collections/sorted_map", Moved::Module("velt/collections")),
    ("compress/brotli", Moved::Module("velt/compress")),
    ("compress/inflate", Moved::Module("velt/compress")),
    ("crypto", Moved::Module("velt/crypto")),
    ("csv", Moved::Module("velt/encoding/csv")),
    ("datetime", Moved::Module("velt/time")),
    ("db", Moved::Module("velt/db")),
    ("dns", Moved::Module("velt/net/dns")),
    ("dom_exception", Moved::Globals),
    ("encoding", Moved::Module("velt/encoding")),
    ("encoding/binary_string", Moved::Module("velt/encoding")),
    ("fetch", Moved::Module("velt/net/http")),
    ("fs", Moved::Module("velt/fs")),
    ("fs_stream", Moved::Module("velt/fs")),
    ("hash", Moved::Module("velt/hash")),
    ("html", Moved::Module("velt/text")),
    ("http", Moved::HttpServer),
    ("io", Moved::Module("velt/io")),
    ("json", Moved::Module("velt/encoding/json")),
    ("jsx", Moved::Module("velt/jsx")),
    ("jsx/jsx-runtime", Moved::Module("velt/jsx/jsx-runtime")),
    ("math", Moved::Module("velt/math")),
    ("mem", Moved::Module("velt/mem")),
    ("net", Moved::Module("velt/net")),
    ("os", Moved::Module("velt/os")),
    ("package", Moved::Module("velt/package")),
    ("path", Moved::Module("velt/path")),
    (
        "postgres",
        Moved::Package("postgres", "the PostgreSQL client"),
    ),
    ("process", Moved::Module("velt/process")),
    ("proxy", Moved::Globals),
    ("random", Moved::Module("velt/random")),
    ("redis", Moved::Package("redis", "the Redis client")),
    ("regex", Moved::Module("velt/text")),
    ("simd", Moved::Module("velt/simd")),
    ("sqlite", Moved::Package("sqlite", "SQLite")),
    ("stdin", Moved::Module("velt/process")),
    ("streams", Moved::Module("velt/io")),
    ("string_builder", Moved::Globals),
    ("symbol", Moved::Globals),
    ("sync", Moved::Module("velt/sync")),
    ("sync/epoch", Moved::Module("velt/sync/epoch")),
    ("task", Moved::Module("velt/task")),
    ("test", Moved::Module("velt/test")),
    ("timers", Moved::Module("velt/time")),
    ("typed_array", Moved::Globals),
    ("udp", Moved::Module("velt/net/udp")),
    ("url", Moved::Module("velt/net/url")),
    ("uuid", Moved::Module("velt/random/uuid")),
    ("websocket", Moved::Module("velt/net/websocket")),
];

/// Where old module `old` went.
pub fn moved(old: &str) -> Option<Moved> {
    MOVED.iter().find(|(o, _)| *o == old).map(|(_, m)| *m)
}

/// Where a package of the Velt repository is, with how to depend on it.
fn package_url(name: &str) -> String {
    format!("https://github.com/velt-lang/velt/tree/main/packages/{name}")
}

/// What became of old module `old`, as a sentence about `written` (the specifier as written):
/// `None` when `old` was no module, or still is `velt/<old>`.
pub fn now(old: &str, written: &str) -> Option<String> {
    Some(match moved(old)? {
        Moved::Module(m)
            if m.strip_prefix("velt/") == Some(old) && written.starts_with("velt/") =>
        {
            return None
        }
        Moved::Module(m) => format!("`{written}` is now `{m}`"),
        Moved::Globals => {
            format!("`{written}` was removed: its names are globals, used without an import")
        }
        Moved::Package(name, what) => format!(
            "`{written}` was removed: {what} is the `{name}` package now, imported as \
             `\"{name}\"` once package.vlt lists it in `dependencies` ({})",
            package_url(name)
        ),
        Moved::HttpServer => format!(
            "`{written}` was removed: the HTTP server is the `http-server` package now, imported \
             as `\"http-server\"` once package.vlt lists it in `dependencies` ({}), and `fetch` \
             and the HTTP types are in `velt/net/http`",
            package_url("http-server")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every module the table names exists in std, and none of the old names is a module still
    /// unless the table says it stayed.
    #[test]
    fn the_new_modules_exist() {
        let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let exists = |rel: &str| {
            std.join(format!("{rel}.vlt")).is_file() || std.join(rel).join("index.vlt").is_file()
        };
        for (old, m) in MOVED {
            if let Moved::Module(new) = m {
                let rel = new.strip_prefix("velt/").expect("a std module");
                assert!(exists(rel), "{old} → {new}");
            }
        }
    }

    #[test]
    fn sentences() {
        assert_eq!(
            now("json", "velt:json").as_deref(),
            Some("`velt:json` is now `velt/encoding/json`")
        );
        assert_eq!(
            now("json", "velt/json").as_deref(),
            Some("`velt/json` is now `velt/encoding/json`")
        );
        // Still there: nothing to say about `velt/fs`, but `velt:fs` is now `velt/fs`.
        assert_eq!(now("fs", "velt/fs"), None);
        assert_eq!(
            now("fs", "velt:fs").as_deref(),
            Some("`velt:fs` is now `velt/fs`")
        );
        assert!(now("sqlite", "velt/sqlite").is_some_and(|s| s.contains("`sqlite` package")));
        assert_eq!(now("nothing", "velt/nothing"), None);
    }
}
