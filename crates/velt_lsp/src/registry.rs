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
/// How long one fetch may take: an editor wants an answer in seconds, not the CLI's minutes, and a
/// stuck registry must not hold a fetch slot for long.
const LIMITS: velt_http::Limits = velt_http::Limits {
    connect: Duration::from_secs(5),
    idle: Duration::from_secs(10),
    total: Duration::from_secs(15),
};
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
            vpm::registry::read_index_within(&loc, &name, LIMITS).map(Value::Index)
        }) {
            Lookup::Ready(Value::Index(index)) => Lookup::Ready(index),
            Lookup::Ready(Value::Search(_)) => unreachable!("ICE: an index slot holds a search"),
            Lookup::Pending => Lookup::Pending,
            Lookup::Unavailable => Lookup::Unavailable,
        }
    }

    /// The packages of `loc`'s registry that match `query` (by name, keywords or description).
    pub fn search(&self, loc: &Locations, query: &str) -> Lookup<Vec<Hit>> {
        let query = query.trim().to_lowercase();
        let key = Key::Search {
            registry: loc.describe(),
            query: query.clone(),
        };
        let loc = loc.clone();
        match self.get(key, move || {
            vpm::search::search_within(&loc, &query, LIMITS).map(Value::Search)
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
/// field, else the local registry. `None` when no registry can be located, and when the
/// manifest names a plain `http://` registry on another machine: opening a checkout must not
/// make the editor talk to hosts it names (an internal network address, say) unless the
/// connection is TLS or stays on this machine. `velt install` still uses such a registry.
pub fn locations_for(manifest_text: &str) -> Option<Locations> {
    let loc = Locations::from_env().ok()?;
    registry_for(loc, manifest_text)
}

fn registry_for(mut loc: Locations, manifest_text: &str) -> Option<Locations> {
    if loc.remote.is_some() {
        return Some(loc); // the user's own $VELT_REGISTRY
    }
    let named = vpm::manifest::ide::registry::top_level_string(manifest_text, "registry")
        .filter(|url| vpm::locations::is_url(url));
    match named {
        Some(url) if !vpm::remote::is_tls_or_loopback(&url) => None,
        named => {
            loc.remote = named;
            Some(loc)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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
    fn a_manifests_registry_is_asked_over_tls_or_on_this_machine_only() {
        let local = Locations::under(Path::new("/h"));
        let with = |url: &str| format!("export const pkg: Package = {{ registry: \"{url}\" }};");
        let remote = |text: &str| registry_for(local.clone(), text).map(|l| l.remote);
        assert_eq!(
            remote(&with("https://r.example")),
            Some(Some("https://r.example".into()))
        );
        assert_eq!(
            remote(&with("http://127.0.0.1:8091")),
            Some(Some("http://127.0.0.1:8091".into()))
        );
        assert_eq!(
            remote(&with("http://10.0.0.5")),
            None,
            "plain http to another machine"
        );
        assert_eq!(
            remote("export const pkg: Package = {};"),
            Some(None),
            "the local registry"
        );
        // `$VELT_REGISTRY` is the user's choice and always wins.
        let mut env = local.clone();
        env.remote = Some("http://10.0.0.5".into());
        assert_eq!(
            registry_for(env, &with("https://r.example"))
                .unwrap()
                .remote
                .as_deref(),
            Some("http://10.0.0.5")
        );
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
