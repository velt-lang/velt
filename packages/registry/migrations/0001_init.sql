-- The registry's tables (D1). Apply with `wrangler d1 migrations apply <database>`.

-- Users and the SHA-256 of their tokens (`sha256:<hex>`); a token is shown once, when made.
CREATE TABLE users (
  name TEXT PRIMARY KEY,
  token TEXT NOT NULL UNIQUE
);

-- Published packages; `norm` is the name with `-` as `_`: names that differ only there are one.
CREATE TABLE packages (
  name TEXT PRIMARY KEY,
  norm TEXT NOT NULL UNIQUE
);

-- Who may publish, yank and change owners of a package.
CREATE TABLE owners (
  package TEXT NOT NULL,
  user TEXT NOT NULL,
  PRIMARY KEY (package, user)
);

-- Every published version, in publication order (`seq`), as the package's index lists it.
-- `keywords`, `dependencies` and `native` are JSON.
CREATE TABLE versions (
  package TEXT NOT NULL,
  version TEXT NOT NULL,
  seq INTEGER NOT NULL,
  checksum TEXT NOT NULL,
  description TEXT,
  keywords TEXT NOT NULL,
  dependencies TEXT NOT NULL,
  native TEXT NOT NULL,
  native_abi INTEGER,
  yanked INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (package, version)
);
