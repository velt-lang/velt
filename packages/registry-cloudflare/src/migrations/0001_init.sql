-- The registry's D1 database. `index_json` is a package's index as `index.json` holds it in
-- every registry (registry-core's `PackageIndex.toJson`).
CREATE TABLE packages (
  name TEXT PRIMARY KEY,
  index_json TEXT NOT NULL
);

-- `position` keeps owners in the order they were added. An owner is a user, so a user who owns
-- packages can't be deleted: a name is never given to someone else with the packages it owns.
CREATE TABLE owners (
  package TEXT NOT NULL,
  position INTEGER NOT NULL,
  user TEXT NOT NULL REFERENCES users (name),
  PRIMARY KEY (package, position)
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
