# registry-core

The package registry's rules in Velt ([#1740](https://github.com/velt-lang/velt/issues/1740)): what
a registry accepts, with the definitions `velt` uses, so a registry written in Velt and the `velt`
client agree. The request handling over pluggable storage (`BlobStore`, `MetadataDb`,
`PublishLock`, `CachePurge`, `AuthProvider`) builds on it, for the Cloudflare host and for
`velt registry serve` alike ([#1325](https://github.com/velt-lang/velt/issues/1325)).

```ts ignore
import {
  readManifest,
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
const manifest = readManifest(text); // ManifestError when vpm would refuse it
```

- **Names** (`src/names.vlt`): package names (lowercase words joined by single `-` or `_`; not
  `std`, `velt`, a native prefix such as `rt-…`, or a Windows device name), scoped dependency
  names (`@scope/name`) and registry user names.
- **Versions** (`src/semver/version.vlt`): semver 2.0 versions, parsed and ordered exactly as the
  Rust `semver` crate does for `velt` (numbers up to `u64`'s largest; build metadata ordered
  too).
- **Requirements** (`src/semver/requirement.vlt`): which strings are version requirements
  (`^1.2`, `>=1, <2`, `1.*`), as the `semver` crate parses them.
- **Manifests** (`src/manifest/`): `package.vlt` read as vpm reads it, never compiled or run:
  one `export const pkg: Package = { … }` of data (strings, booleans, arrays, objects), every
  field checked; `readManifest(text)` gives a `Manifest` or throws `ManifestError` with every
  problem, and `Manifest.toJson()` is `velt manifest --json`.
- **Archives** (`src/archive.vlt`, `src/package_archive.vlt`): the `VELTPKG1` archive `velt
  publish` uploads, its path rules (`readPackageArchive` allows the native crate's directory its
  manifest declares), and the content checksum of its files.

`crates/veltc/tests/registry_core.rs` checks every rule against the Rust it must agree with
(vpm, the `semver` crate) over some 5,000 listed and generated cases, among them manifests
mutated token by token.
