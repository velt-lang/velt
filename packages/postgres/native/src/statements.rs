//! The per-connection prepared-statement cache: SQL text → server-side prepared statement (and,
//! for named parameters, the rewritten SQL's placeholder names), least recently used evicted
//! beyond [`CAPACITY`]. An evicted `Statement` is closed on the server when its last user
//! drops it (and so is its batch twin, see `super::batch`).

use super::batch::BatchStatement;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_postgres::Statement;

/// Statements kept prepared per connection.
pub const CAPACITY: usize = 256;

/// A cached statement.
#[derive(Clone)]
pub struct Prepared {
    /// The server-side statement (parameter and column types).
    pub statement: Statement,
    /// Placeholder names in `$n` order when the SQL used named parameters.
    pub names: Option<Arc<[String]>>,
    /// The same statement as batches prepare it on the server.
    pub batch: Arc<BatchStatement>,
}

/// Key: the SQL text and whether it was prepared for named parameters (the same text binds
/// differently for an object and an array).
type Key = (String, bool);

/// LRU map of prepared statements.
#[derive(Default)]
pub struct StatementCache {
    entries: HashMap<Key, (Prepared, u64)>,
    clock: u64,
}

impl StatementCache {
    /// The statement for `sql`, marking it recently used.
    pub fn get(&mut self, sql: &str, named: bool) -> Option<Prepared> {
        self.clock += 1;
        let clock = self.clock;
        // Borrowed lookup would need a custom key type; the clone is cheap next to a query.
        let entry = self.entries.get_mut(&(sql.to_string(), named))?;
        entry.1 = clock;
        Some(entry.0.clone())
    }

    /// Cache `prepared` for `sql`, evicting the least recently used entry when full.
    pub fn insert(&mut self, sql: &str, named: bool, prepared: Prepared) {
        if self.entries.len() >= CAPACITY {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            }
        }
        self.clock += 1;
        self.entries
            .insert((sql.to_string(), named), (prepared, self.clock));
    }

    /// Forget `sql` (its plan went stale on the server).
    pub fn remove(&mut self, sql: &str, named: bool) {
        self.entries.remove(&(sql.to_string(), named));
    }
}
