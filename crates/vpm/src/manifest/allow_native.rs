//! An `allowNative` entry (#2367): a package whose native code the program allows, and the source
//! it is allowed from. A registry name is the registry's, but a git repository or an archive
//! names its package whatever it likes, so trust in those binds to where they come from:
//!
//! ```ts ignore
//! allowNative: [
//!   "sqlite",                                              // the registry, or a path the root wrote
//!   { name: "llama", git: "https://github.com/o/llama" },  // that repository, any commit
//!   { name: "duck", url: "https://github.com/o/duck/releases/download/" },  // under that prefix
//! ]
//! ```
//!
//! [`crate::install`] checks each package with native code against these after resolution
//! (`install/allow_native.rs`); the URLs compare as [`crate::source_url`] says.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One `allowNative` entry.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllowNative {
    /// The package's name.
    pub name: String,
    /// Where it is allowed from.
    pub from: AllowedFrom,
}

/// The sources an [`AllowNative`] entry accepts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllowedFrom {
    /// A bare name: the registry, or a `path` that a manifest the root controls wrote.
    Root,
    /// `git`: that repository (as written), at any commit and directory.
    Git(String),
    /// `url`: archives under that prefix (as written).
    Url(String),
}

impl AllowNative {
    /// A bare-name entry.
    pub fn named(name: impl Into<String>) -> AllowNative {
        AllowNative {
            name: name.into(),
            from: AllowedFrom::Root,
        }
    }

    /// The entry as package.velt writes it: `"sqlite"`, `{ name: "llama", git: "…" }`.
    pub fn to_source(&self) -> String {
        let quoted = super::write::string_lit;
        match &self.from {
            AllowedFrom::Root => quoted(&self.name),
            AllowedFrom::Git(url) => {
                format!("{{ name: {}, git: {} }}", quoted(&self.name), quoted(url))
            }
            AllowedFrom::Url(url) => {
                format!("{{ name: {}, url: {} }}", quoted(&self.name), quoted(url))
            }
        }
    }

    /// The entry as JSON (`velt manifest --json`, `velt.lock.json`).
    pub fn to_json(&self) -> serde_json::Value {
        match &self.from {
            AllowedFrom::Root => serde_json::Value::String(self.name.clone()),
            AllowedFrom::Git(url) => serde_json::json!({ "name": self.name, "git": url }),
            AllowedFrom::Url(url) => serde_json::json!({ "name": self.name, "url": url }),
        }
    }

    /// Where the entry allows the package from, for messages: `the registry or a local path`,
    /// `the git repository https://…`, `URLs under https://…`.
    pub fn describe_from(&self) -> String {
        match &self.from {
            AllowedFrom::Root => "the registry or a local path".to_string(),
            AllowedFrom::Git(url) => format!("the git repository {url}"),
            AllowedFrom::Url(prefix) => format!("URLs under {prefix}"),
        }
    }
}

impl From<&str> for AllowNative {
    fn from(name: &str) -> AllowNative {
        AllowNative::named(name)
    }
}

impl PartialEq<&str> for AllowNative {
    fn eq(&self, name: &&str) -> bool {
        self.from == AllowedFrom::Root && self.name == *name
    }
}

impl Serialize for AllowNative {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        /// An entry with a source, `name` first.
        #[derive(Serialize)]
        struct Entry<'a> {
            name: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            git: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            url: Option<&'a str>,
        }
        let (git, url) = match &self.from {
            AllowedFrom::Root => return self.name.serialize(s),
            AllowedFrom::Git(git) => (Some(git.as_str()), None),
            AllowedFrom::Url(url) => (None, Some(url.as_str())),
        };
        Entry {
            name: &self.name,
            git,
            url,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for AllowNative {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<AllowNative, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Name(String),
            Entry(Entry),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Entry {
            name: String,
            git: Option<String>,
            url: Option<String>,
        }
        Ok(match Written::deserialize(d)? {
            Written::Name(name) => AllowNative::named(name),
            Written::Entry(Entry { name, git, url }) => {
                let from = match (git, url) {
                    (Some(git), None) => AllowedFrom::Git(git),
                    (None, Some(url)) => AllowedFrom::Url(url),
                    _ => {
                        return Err(D::Error::custom(
                            "an allowNative entry has exactly one of `git` and `url`",
                        ))
                    }
                };
                AllowNative { name, from }
            }
        })
    }
}

/// Check an entry's source as package.velt writes it: a git URL that compares safely, or an https
/// prefix with a path under its host.
pub(crate) fn check_from(from: &AllowedFrom) -> Result<(), String> {
    match from {
        AllowedFrom::Root => Ok(()),
        AllowedFrom::Git(url) => crate::source_url::git_identity(url).map(drop),
        AllowedFrom::Url(prefix) => crate::source_url::check_prefix(prefix),
    }
}
