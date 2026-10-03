//! Registry data for `package.vlt` (package indexes and name searches), fetched on background
//! threads so that typing never waits for the network, and cached for the session. A lookup
//! answers at once: the data, "not yet" (a fetch is running; the server asks again shortly), or
//! "unavailable" (the registry could not be reached: the editor stays quiet about it).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vpm::registry::Index;
use vpm::search::Hit;
use vpm::Locations;

/// How long fetched data is used before it is fetched again (in the background, while the old
/// data keeps answering).
const FRESH: Duration = Duration::from_secs(300);
/// Most fetches running at once (a slow registry must not collect a thread per keystroke).
const MAX_FETCHES: usize = 4;

/// The answer to a lookup.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup<T> {
    Ready(T),
    /// A fetch is running.
    Pending,
    /// The registry could not be asked.
    Unavailable,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Index { registry: String, name: String },
    Search { registry: String, query: String },
}

#[derive(Clone)]
enum Value {
    Index(Option<Index>),
    Search(Vec<Hit>),
}

#[derive(Default)]
struct Slot {
    /// The last answer (`Err`: the fetch failed) and when it came.
    value: Option<(Result<Value, ()>, Instant)>,
    fetching: bool,
}

/// The session's registry data.
#[derive(Clone, Default)]
pub struct RegistryData {
    slots: Arc<Mutex<HashMap<Key, Slot>>>,
}

impl RegistryData {
    /// The index of package `name` in `loc`'s registry (`None`: no such package).
    pub fn index(&self, loc: &Locations, name: &str) -> Lookup<Option<Index>> {
        if !vpm::manifest::is_valid_package_name(name) {
            return Lookup::Unavailable; // never a path or URL segment of its own
        }
        let key = Key::Index {
            registry: loc.describe(),
            name: name.to_string(),
        };
        let (loc, name) = (loc.clone(), name.to_string());
        match self.get(key, move || {
            vpm::registry::read_index(&loc, &name).map(Value::Index)
        }) {
            Lookup::Ready(Value::Index(index)) => Lookup::Ready(index),
            Lookup::Ready(Value::Search(_)) => unreachable!("ICE: an index slot holds a search"),
            Lookup::Pending => Lookup::Pending,
            Lookup::Unavailable => Lookup::Unavailable,
        }
    }

    /// The packages of `loc`'s registry whose name contains `query`.
    pub fn search(&self, loc: &Locations, query: &str) -> Lookup<Vec<Hit>> {
        let query = query.trim().to_lowercase();
        let key = Key::Search {
            registry: loc.describe(),
            query: query.clone(),
        };
        let loc = loc.clone();
        match self.get(key, move || {
            vpm::search::search(&loc, &query).map(Value::Search)
        }) {
            Lookup::Ready(Value::Search(hits)) => Lookup::Ready(hits),
            Lookup::Ready(Value::Index(_)) => unreachable!("ICE: a search slot holds an index"),
            Lookup::Pending => Lookup::Pending,
            Lookup::Unavailable => Lookup::Unavailable,
        }
    }

    /// Whether a fetch is running.
    pub fn busy(&self) -> bool {
        self.lock().values().any(|s| s.fetching)
    }

    /// `lookup()` again until it is no longer pending, for at most `limit` (completion waits a
    /// little for data already on its way: editors do not ask again by themselves).
    pub fn settle<T>(&self, limit: Duration, lookup: impl Fn() -> Lookup<T>) -> Lookup<T> {
        let deadline = Instant::now() + limit;
        loop {
            let answer = lookup();
            if !matches!(answer, Lookup::Pending) || Instant::now() >= deadline {
                return answer;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cached value of `key`, starting `fetch` in the background when there is none or it
    /// is stale.
    fn get(
        &self,
        key: Key,
        fetch: impl FnOnce() -> Result<Value, String> + Send + 'static,
    ) -> Lookup<Value> {
        let mut slots = self.lock();
        let running = slots.values().filter(|s| s.fetching).count();
        let slot = slots.entry(key.clone()).or_default();
        let stale = slot
            .value
            .as_ref()
            .is_none_or(|(_, at)| at.elapsed() > FRESH);
        if stale && !slot.fetching && running < MAX_FETCHES {
            slot.fetching = true;
            let slots = self.slots.clone();
            let spawned = std::thread::Builder::new()
                .name("velt-lsp-registry".into())
                .spawn(move || {
                    // A panicking fetch must still end the fetch (else the key stays pending and
                    // the server keeps re-checking).
                    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(fetch))
                        .map_err(|_| ())
                        .and_then(|r| r.map_err(|_| ()));
                    let mut slots = slots.lock().unwrap_or_else(|e| e.into_inner());
                    let slot = slots.entry(key).or_default();
                    slot.value = Some((value, Instant::now()));
                    slot.fetching = false;
                });
            if spawned.is_err() {
                slot.fetching = false;
                slot.value = Some((Err(()), Instant::now()));
            }
        }
        match &slot.value {
            Some((Ok(v), _)) => Lookup::Ready(v.clone()),
            Some((Err(()), _)) if !slot.fetching => Lookup::Unavailable,
            _ => Lookup::Pending,
        }
    }
}

/// The registry a manifest's dependencies come from: `$VELT_REGISTRY`, else its `registry`
/// field, else the local registry. `None` when no registry can be located.
pub fn locations_for(manifest_text: &str) -> Option<Locations> {
    let mut loc = Locations::from_env().ok()?;
    if loc.remote.is_none() {
        loc.remote = vpm::manifest::ide::registry::top_level_string(manifest_text, "registry")
            .filter(|url| vpm::locations::is_url(url));
    }
    Some(loc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_is_pending_then_ready_and_cached() {
        let tmp = std::env::temp_dir().join(format!("velt_lsp_registry_{}", std::process::id()));
        let loc = Locations::under(&tmp);
        let data = RegistryData::default();
        let mut first = data.index(&loc, "nothing-here");
        let deadline = Instant::now() + Duration::from_secs(10);
        while first == Lookup::Pending && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            first = data.index(&loc, "nothing-here");
        }
        // The local registry directory does not exist: no such package.
        assert_eq!(first, Lookup::Ready(None));
        assert!(!data.busy());
        assert_eq!(data.index(&loc, "nothing-here"), Lookup::Ready(None));
        // A name that is not a package name is never looked up.
        assert_eq!(data.index(&loc, "../x"), Lookup::Unavailable);
    }

    #[test]
    fn a_panicking_fetch_ends_as_unavailable() {
        let data = RegistryData::default();
        let key = Key::Search {
            registry: "test".into(),
            query: "boom".into(),
        };
        let fetch = || -> Result<Value, String> { panic!("a broken registry answer") };
        let mut answer = data.get(key.clone(), fetch);
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(answer, Lookup::Pending) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            answer = data.get(key.clone(), || unreachable!("one fetch per key"));
        }
        assert!(matches!(answer, Lookup::Unavailable));
        assert!(!data.busy(), "the server must stop re-checking");
    }
}
