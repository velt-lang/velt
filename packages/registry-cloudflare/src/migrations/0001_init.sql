-- The registry's D1 database: small rows only. Clients read each package's index as a static file
-- in R2, which the Worker writes from these rows after each write, so reads never reach here.

-- `revision` rises by one with each committed write: a write commits only from the revision it read
-- (compare-and-set). `normalized` is registry-core's `normalizedName`: one per package.
CREATE TABLE packages (
  name TEXT PRIMARY KEY,
  normalized TEXT NOT NULL UNIQUE,
  revision INTEGER NOT NULL
);

-- One row per version, `entry_json` its entry as `index.json` has it; `position` keeps them in
-- publication order.
CREATE TABLE versions (
  package TEXT NOT NULL,
  version TEXT NOT NULL,
  position INTEGER NOT NULL,
  entry_json TEXT NOT NULL,
  PRIMARY KEY (package, version)
);

-- `position` keeps owners in the order they were added. An owner is a user, so a user who owns
-- packages can't be deleted: a name is never given to someone else with the packages it owns.
CREATE TABLE owners (
  package TEXT NOT NULL,
  position INTEGER NOT NULL,
  user TEXT NOT NULL REFERENCES users (name),
  PRIMARY KEY (package, position)
);

-- The entry a search shows for each package (the version `velt add` picks); none when every
-- version is yanked.
CREATE TABLE search (
  package TEXT PRIMARY KEY,
  entry_json TEXT NOT NULL
);

-- `github_id` is the GitHub account a user signs in with (null for one an administrator added):
-- a user keeps their name when they rename the account. A user is never deleted, so a name is
-- never reused; taking someone's access away deletes their tokens.
CREATE TABLE users (
  name TEXT PRIMARY KEY,
  github_id INTEGER UNIQUE
);

-- A user's tokens: only the hash of each (`sha256:<hex>`), never the token. `created` is in
-- milliseconds since 1970.
CREATE TABLE tokens (
  token_hash TEXT PRIMARY KEY,
  user TEXT NOT NULL REFERENCES users (name),
  created INTEGER NOT NULL
);
