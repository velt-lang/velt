# registry-core

The package registry's rules in Velt ([#1740](https://github.com/velt-lang/velt/issues/1740)): what
a registry accepts, with the definitions `velt` uses, so a registry written in Velt and the `velt`
client agree. The request handling over pluggable storage (`BlobStore`, `MetadataDb`,
`PublishLock`, `CachePurge`, `AuthProvider`) builds on it, for the Cloudflare host and for
`velt registry serve` alike ([#1325](https://github.com/velt-lang/velt/issues/1325)).

```ts ignore
import {
  checkName,
  checkPackagePaths,
  contentChecksum,
  isValidPackageName,
  readArchive,
  Version,
} from "registry-core";

isValidPackageName("my-pkg"); // true; checkName("std") says why a name is refused
Version.parse("1.2.3-rc.1").compare(Version.parse("1.2.3")); // negative: a pre-release orders first

const entries = readArchive(body, (path: string): bool => true); // ArchiveError when malformed
checkPackagePaths(entries, null); // only package.vlt, src/ and the native crate's directory
const sum = contentChecksum(entries); // "sha256:…", as `velt` records it
```

- **Names** (`src/names.vlt`): package names (lowercase words joined by single `-` or `_`; not
  `std`, `velt`, a native prefix such as `rt-…`, or a Windows device name), scoped dependency
  names (`@scope/name`) and registry user names.
- **Versions** (`src/semver/version.vlt`): semver 2.0 versions, parsed and ordered exactly as the
  Rust `semver` crate does for `velt` (numbers up to `u64`'s largest; build metadata ordered
  too).
- **Archives** (`src/archive.vlt`): the `VELTPKG1` archive `velt publish` uploads, its path rules,
  and the content checksum of its files.

`crates/veltc/tests/registry_core.rs` checks every rule against the Rust it must agree with
(vpm, the `semver` crate) over some 3,000 listed and generated cases.
