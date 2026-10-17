//! The Rust counterpart of `bench/sqlite/rows.vlt`: the same in-memory database, queries and
//! output, with rusqlite (bundled SQLite, cached statements), serde_json and mimalloc (the Velt
//! runtime's allocator). Prints the bytes written, which must equal the Velt program's. Usage: rows [feed|post|both] [rounds]

use rusqlite::Connection;
use serde::Serialize;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const POST_COLUMNS: &str = "SELECT p.id, p.body, p.created_at, u.username AS author, (SELECT count(*) FROM likes l WHERE l.post_id = p.id) AS like_count FROM posts p JOIN users u ON u.id = p.user_id";
const POSTS: i64 = 2000;

#[derive(Serialize)]
struct Post {
    id: i64,
    body: String,
    created_at: String,
    author: String,
    like_count: i64,
}

#[derive(Serialize)]
struct Feed {
    posts: Vec<Post>,
}

fn post(r: &rusqlite::Row) -> rusqlite::Result<Post> {
    Ok(Post {
        id: r.get(0)?,
        body: r.get(1)?,
        created_at: r.get(2)?,
        author: r.get(3)?,
        like_count: r.get(4)?,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let which = args.first().map_or("both", String::as_str);
    let rounds: i64 = args.get(1).map_or(2000, |n| n.parse().expect("rounds"));
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(SETUP).unwrap();
    let feed = format!("{POST_COLUMNS} ORDER BY p.created_at DESC, p.id DESC LIMIT 20");
    let one = format!("{POST_COLUMNS} WHERE p.id = ?");
    let mut bytes = 0;
    if which != "post" {
        for _ in 0..rounds {
            let mut st = db.prepare_cached(&feed).unwrap();
            let posts = st
                .query_map([], post)
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            bytes += serde_json::to_string(&Feed { posts }).unwrap().len();
        }
    }
    if which != "feed" {
        for i in 0..rounds * 10 {
            let mut st = db.prepare_cached(&one).unwrap();
            if let Ok(p) = st.query_row([(i * 7919) % POSTS + 1], post) {
                bytes += serde_json::to_string(&p).unwrap().len();
            }
        }
    }
    println!("{bytes}");
}

const SETUP: &str = "
    CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL);
    CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL, body TEXT NOT NULL,
      created_at TEXT NOT NULL);
    CREATE INDEX posts_created ON posts (created_at DESC, id DESC);
    CREATE TABLE likes (user_id INTEGER NOT NULL, post_id INTEGER NOT NULL,
      PRIMARY KEY (user_id, post_id));
    CREATE INDEX likes_post ON likes (post_id);
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100)
      INSERT INTO users SELECT i, 'user_' || i FROM n;
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
      INSERT INTO posts SELECT i, i % 100 + 1,
        substr(replace(hex(zeroblob(40)), '00', 'post ' || i || ' says '), 1, 180 + i % 40),
        strftime('%Y-%m-%dT%H:%M:%fZ', 1700000000 + i * 60, 'unixepoch') FROM n;
    WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
      INSERT OR IGNORE INTO likes SELECT i % 100 + 1, (i * 7) % 2000 + 1 FROM n;
  ";
