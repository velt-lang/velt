//! Topics (publish/subscribe) for `std/websocket`: `ws.subscribe(topic)`, `publish(topic, msg)`.
//!
//! A publish encodes the frame once and appends the same bytes to every subscriber's queue (the
//! writer task of each connection sends it with whatever else is queued), so a message to many
//! sockets costs one encode, one queue append per socket and no task per socket. A subscriber
//! with more than [`PUBLISH_LIMIT`] bytes already queued is skipped for that message, so one
//! slow reader cannot grow the server's memory without bound (a direct `send` waits instead).
//! Topics are process-wide; a connection leaves all of them when it is dropped.

use super::frame::{self, Op};
use super::{Conn, Role, Writer};

use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Queued bytes past which a subscriber misses published messages.
pub(super) const PUBLISH_LIMIT: usize = 16 << 20;

struct Sub {
    conn: u64,
    role: Role,
    tx: Arc<Writer>,
}

type Topic = Arc<Mutex<Vec<Sub>>>;

static TOPICS: LazyLock<RwLock<HashMap<String, Topic>>> = LazyLock::new(Default::default);

/// Adds `conn` to `topic` (again is a no-op); true if it was not subscribed yet.
pub(super) fn subscribe(conn: &Conn, topic: &str) -> bool {
    // The map stays locked while joining: an `unsubscribe` that empties the topic removes it
    // under the write lock, so it cannot remove the list this connection is joining (which
    // every later publish would miss). Lock order: the map, then the topic.
    {
        let all = TOPICS.read();
        if let Some(t) = all.get(topic) {
            return join(&mut t.lock(), conn);
        }
    }
    let mut all = TOPICS.write();
    let t = all.entry(topic.to_string()).or_default();
    let joined = join(&mut t.lock(), conn);
    joined
}

fn join(subs: &mut Vec<Sub>, conn: &Conn) -> bool {
    if subs.iter().any(|s| s.conn == conn.id) {
        return false;
    }
    subs.push(Sub {
        conn: conn.id,
        role: conn.tx.role,
        tx: conn.tx.clone(),
    });
    true
}

/// Removes connection `id` from `topic`; an emptied topic goes away.
pub(super) fn unsubscribe(id: u64, topic: &str) {
    let Some(t) = TOPICS.read().get(topic).cloned() else {
        return;
    };
    let empty = {
        let mut subs = t.lock();
        subs.retain(|s| s.conn != id);
        subs.is_empty()
    };
    if empty {
        let mut all = TOPICS.write();
        // Someone may have subscribed in between: only remove the topic if still empty.
        if all.get(topic).is_some_and(|t| t.lock().is_empty()) {
            all.remove(topic);
        }
    }
}

/// Queues `payload` as one `op` frame for every subscriber of `topic` except connection
/// `except` (0: none); the number of connections it was queued for.
pub(super) fn publish(topic: &str, op: Op, payload: &[u8], except: u64) -> u64 {
    let Some(t) = TOPICS.read().get(topic).cloned() else {
        return 0;
    };
    let mut server_frame = Vec::new();
    frame::encode(&mut server_frame, op, payload, false);
    let subs = t.lock();
    let mut sent = 0;
    for s in subs.iter().filter(|s| s.conn != except) {
        let queued = match s.role {
            Role::Server => s.tx.append(&server_frame, PUBLISH_LIMIT),
            // A client masks every frame with a key of its own.
            Role::Client => {
                let mut f = Vec::new();
                frame::encode(&mut f, op, payload, true);
                s.tx.append(&f, PUBLISH_LIMIT)
            }
        };
        sent += queued as u64;
    }
    sent
}
