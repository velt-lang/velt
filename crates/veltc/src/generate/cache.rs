//! Caching generators' work (docs/internals/design/source-generators.md §9), in the build cache:
//!
//! - **output**: what the host wrote for a round's input, by a key over the compiler, the host
//!   program and the input (the requests and their model). A generator is a pure function of its
//!   request, so equal keys mean equal output while the files the host's build read (the
//!   generators, what they import) are unchanged; those are recorded as build-stamp entries
//!   and checked like a stamp (by size and time, hashed only when those differ);
//! - **offers**: per program (its root file), the generated modules its last build used, with
//!   each instance's fingerprint. The driver offers them to the first check, which then needs no
//!   second round when nothing they were generated for changed.

use std::path::Path;

use serde_json::{json, Value};
use velt_build_cache::{Key, KeyHasher};
use velt_sema::generated::GenInstance;

use crate::driver::stamp;

/// The build cache's kinds for generator output and for offers.
const OUTPUT_KIND: &str = "generated";
const OFFER_KIND: &str = "generated-offer";

/// The key of a round's output: the compiler, the host program (which generators) and the
/// requests with their model.
pub(super) fn output_key(root: &Path, src: &str, input: &str) -> Option<Key> {
    crate::build_cache::cache()?;
    let mut h = KeyHasher::new();
    h.part(b"generate-output-v4");
    h.part(&crate::build_cache::compiler_identity().0);
    h.part(root.as_os_str().as_encoded_bytes());
    h.part(src.as_bytes());
    h.part(input.as_bytes());
    Some(h.finish())
}

/// The host's output for `key` and the files its build read, when those are unchanged.
pub(super) fn cached_output(key: &Key) -> Option<(Vec<u8>, Vec<String>)> {
    let entry = crate::build_cache::cache()?.get_key(OUTPUT_KIND, key)?;
    let (files, rest) = read_files(&entry, key)?;
    Some((rest.to_vec(), files))
}

/// Store the host's output for `key`; `files`: what its build read (none: not cached, as its
/// generators could change unseen).
pub(super) fn store_output(key: &Key, output: &[u8], files: &[String]) {
    let Some(cache) = crate::build_cache::cache() else {
        return;
    };
    if files.is_empty() {
        return;
    }
    let mut bytes = files_record(key, files).into_bytes();
    bytes.extend_from_slice(output);
    cache.put_key(OUTPUT_KIND, key, &bytes);
}

/// `<written>\n<length>\n<stamp>`: a build stamp of `files` keyed by `key`, written now.
fn files_record(key: &Key, files: &[String]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let stamp = stamp::compose(key, 0, files);
    format!("{now}\n{}\n{stamp}", stamp.len())
}

/// The files of a [`files_record`] at the start of `entry` when they are unchanged, and the
/// rest of the entry.
fn read_files<'a>(entry: &'a [u8], key: &Key) -> Option<(Vec<String>, &'a [u8])> {
    let mut pos = 0;
    let mut line = || {
        let end = pos + entry[pos..].iter().position(|&b| b == b'\n')?;
        let text = std::str::from_utf8(&entry[pos..end]).ok()?;
        pos = end + 1;
        Some(text)
    };
    let written: u128 = line()?.parse().ok()?;
    let len: usize = line()?.parse().ok()?;
    let text = std::str::from_utf8(entry.get(pos..pos + len)?).ok()?;
    stamp::check(text, key, Some(written))?;
    let files = stamp::entry_lines(text)
        .into_iter()
        .map(String::from)
        .collect();
    Some((files, &entry[pos + len..]))
}

/// What a build generated, to offer to the next build of the same program.
#[derive(Default)]
pub(super) struct Offer {
    pub files: Vec<OfferFile>,
    /// The instances in them, with their fingerprints.
    pub instances: Vec<GenInstance>,
    /// What the generators' builds read (build-stamp entries).
    pub generator_files: Vec<String>,
}

/// A generated module of an offer.
pub(super) struct OfferFile {
    /// The declaring module's path and the generated module's.
    pub module: String,
    pub path: String,
    pub text: String,
    /// [`velt_sema::generated::GenModule::access`].
    pub access: Vec<String>,
}

/// The key of program `root`'s offer.
pub(super) fn offer_key(root: &Path, require_main: bool) -> Option<Key> {
    crate::build_cache::cache()?;
    let mut h = KeyHasher::new();
    h.part(b"generate-offer-v3");
    h.part(&crate::build_cache::compiler_identity().0);
    h.part(root.as_os_str().as_encoded_bytes());
    h.part(&[u8::from(require_main)]);
    Some(h.finish())
}

/// The offer stored for `key`, when the generators it came from are unchanged.
pub(super) fn load_offer(key: &Key) -> Option<Offer> {
    let entry = crate::build_cache::cache()?.get_key(OFFER_KIND, key)?;
    let (generator_files, rest) = read_files(&entry, key)?;
    let v: Value = serde_json::from_slice(rest).ok()?;
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let mut offer = Offer {
        generator_files,
        ..Offer::default()
    };
    let strings = |v: &Value, k: &str| {
        (v.get(k)?.as_array()?.iter())
            .map(|a| a.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
    };
    for f in v.get("files")?.as_array()? {
        offer.files.push(OfferFile {
            module: text(f, "module")?,
            path: text(f, "path")?,
            text: text(f, "text")?,
            access: strings(f, "access")?,
        });
    }
    for i in v.get("instances")?.as_array()? {
        offer.instances.push(GenInstance {
            module: text(i, "module")?,
            function: text(i, "function")?,
            type_args: strings(i, "typeArgs")?,
            body: text(i, "body")?,
            body_module: text(i, "bodyModule")?,
            fingerprint: text(i, "fingerprint")?,
        });
    }
    Some(offer)
}

/// Store `offer` for `key` (not when a generator's files are unknown: it could not be checked).
pub(super) fn store_offer(key: &Key, offer: &Offer) {
    let Some(cache) = crate::build_cache::cache() else {
        return;
    };
    if offer.generator_files.is_empty() {
        return;
    }
    let v = json!({
        "files": offer.files.iter().map(|f| json!({
            "module": f.module, "path": f.path, "text": f.text, "access": f.access,
        })).collect::<Vec<_>>(),
        "instances": offer.instances.iter().map(|i| json!({
            "module": i.module,
            "function": i.function,
            "typeArgs": i.type_args,
            "body": i.body,
            "bodyModule": i.body_module,
            "fingerprint": i.fingerprint,
        })).collect::<Vec<_>>(),
    });
    let mut bytes = files_record(key, &offer.generator_files).into_bytes();
    bytes.extend_from_slice(v.to_string().as_bytes());
    cache.put_key(OFFER_KIND, key, &bytes);
}
