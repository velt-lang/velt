-- The registry's D1 database. `index_json` is a package's index as `index.json` holds it in
-- every registry (registry-core's `PackageIndex.toJson`).
CREATE TABLE packages (
  name TEXT PRIMARY KEY,
  index_json TEXT NOT NULL
);

-- `position` keeps owners in the order they were added.
CREATE TABLE owners (
  package TEXT NOT NULL,
  position INTEGER NOT NULL,
  user TEXT NOT NULL,
  PRIMARY KEY (package, position)
);

-- Only the hash of a token (`sha256:<hex>`), never the token.
CREATE TABLE users (
  name TEXT PRIMARY KEY,
  token_hash TEXT NOT NULL UNIQUE
);
