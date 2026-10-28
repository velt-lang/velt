-- GitHub sign-in (src/github.vlt): a user made by signing in keeps GitHub's numeric id, so a
-- renamed GitHub account keeps its registry user; and the `state` of each sign-in in progress.
ALTER TABLE users ADD COLUMN github_id INTEGER;
CREATE UNIQUE INDEX users_github_id ON users (github_id);

CREATE TABLE oauth_states (
  state TEXT PRIMARY KEY,
  created INTEGER NOT NULL
);
