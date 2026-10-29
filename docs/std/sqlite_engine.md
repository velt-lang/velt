# velt:sqlite/engine

> In development (#1726): the SQLite engine written in Velt, which `velt:sqlite` will move onto
> once it writes. It reads databases today; INSERT, UPDATE, DELETE and DDL come next.

`import { openDatabase, Database, Statement, SqliteError } from "velt:sqlite/engine"`. Queries
over a SQLite database file, with no C library: the file format, the SQL parser, the planner
and the execution are Velt (`velt:sqlite/format`, `btree`, `sql/*`, `exec/*`). Answers are
SQLite's, value for value: type affinity, collations, NULL rules, integer overflow to REAL and
the text form of REAL values are kept, and the tests compare it with C SQLite. Rows a query
does not order come in the order SQLite's plan gives them where this engine picks the same plan
(the same index, the same scan direction); where SQLite's cost model would pick another plan
they can come in another order, and DISTINCT or GROUP BY can then show another row of a group
for its bare columns.

- `openDatabase(path): Database` reads the file into memory; `new Database(bytes)` takes the
  bytes of a database file. A file that is not a database throws `SQLITE_CORRUPT`, a missing
  one `SQLITE_CANTOPEN`.
- `Database.prepare(sql): Statement` parses and plans one SELECT (a trailing `;` is fine).
  Plans are cached by their text (the 200 used last), so preparing the same SQL again is a
  lookup. SQL the engine can't run yet fails here with a message naming it.
- `Statement`: `columns(): string[]`, the result's column names; `values(): Val[]`, every
  row's values (`velt:sqlite/exec/value`'s `Val`: its storage class `t` and the field for it),
  `columns().length` a row; `plan(): string[]`, how each FROM item is read, in the words of
  SQLite's `EXPLAIN QUERY PLAN` (`SEARCH posts USING INTEGER PRIMARY KEY (rowid=?)`).
- `SqliteError { code, message }`: `SQLITE_ERROR` (unknown table, column or function, wrong
  arguments, an error while running such as `integer overflow` in `sum()`), `SQLITE_CORRUPT`,
  `SQLITE_CANTOPEN`. Messages are SQLite's
  (`no such column: x`, `misuse of aggregate: count()`).

```ts
import { open } from "velt:sqlite";
import { openDatabase } from "velt:sqlite/engine";

function main() {
  // Written by C SQLite, read by the Velt engine.
  const c = open("blog.db");
  c.exec(`
    CREATE TABLE IF NOT EXISTS posts (id INTEGER PRIMARY KEY, title TEXT);
    CREATE TABLE IF NOT EXISTS likes (post_id INTEGER);
    CREATE INDEX IF NOT EXISTS likes_post ON likes (post_id);
  `);
  c.close();
  const db = openDatabase("blog.db");
  const st = db.prepare(`SELECT p.id, p.title, (SELECT count(*) FROM likes l WHERE l.post_id = p.id) AS likes
                         FROM posts p ORDER BY likes DESC, p.id LIMIT 10`);
  const vals = st.values();
  for (let i: usize = 0; i + 3 <= vals.length; i += 3) {
    console.log(vals[i].i, vals[i + 1].s, vals[i + 2].i);
  }
}
```

## What SQL runs

SELECT with: joins (inner, cross, LEFT, `USING`, `NATURAL`), WHERE, GROUP BY, HAVING, DISTINCT,
ORDER BY (with `NULLS FIRST`/`LAST`, collations, result column numbers and aliases), LIMIT and
OFFSET; views; subqueries in FROM, scalar subqueries, `IN (SELECT …)`, `EXISTS`, correlated ones;
`WITH` (common table expressions, `WITH RECURSIVE` with a queue as SQLite runs it); compound selects (UNION, UNION ALL,
INTERSECT, EXCEPT); VALUES. Expressions: every operator, `CASE`, `CAST`, `COLLATE` (BINARY,
NOCASE, RTRIM), `LIKE … ESCAPE`, `GLOB`, `BETWEEN`, `IN (list)`, `IS [NOT] [DISTINCT FROM]`.
Functions: `abs`, `char`, `coalesce`, `concat`, `concat_ws`, `glob`, `hex`, `ifnull`, `iif`,
`instr`, `length`, `like`, `likelihood`, `likely`, `lower`, `ltrim`, `max`, `min`, `nullif`,
`octet_length`, `printf` (and `format`), `quote`, `replace`, `round`, `rtrim`, `sign`, `substr`, `substring`, `trim`,
`typeof`, `unicode`, `unlikely`, `upper`, `zeroblob`; date and time: `date`, `time`,
`datetime`, `julianday`, `unixepoch`, `strftime`, `current_date`, `current_time`,
`current_timestamp`, with SQLite's modifiers (but `localtime` and `utc`); aggregates `avg`, `count`,
`group_concat`, `max`, `min`, `string_agg`, `sum`, `total`, with `DISTINCT` and `FILTER`.

Not yet (an error at `prepare`): statements other than SELECT, RIGHT and FULL joins, ORDER BY
inside a recursive CTE, window functions, the `localtime`
and `utc` date modifiers, `timediff`, ORDER BY inside an aggregate (`group_concat(x ORDER BY y)`),
the math functions (`sqrt`, `pow`, `ln`, `log`, `floor`, `ceil`, `pi`, the trigonometric ones),
`median` and `percentile`, `unhex`, `unistr`, JSON functions and operators, `random()`, tables
`WITHOUT ROWID`, table-valued functions, row values, OR terms over several indexes (SQLite's
MULTI-INDEX OR: such a query runs, with a full scan). Nesting: views, subqueries and CTEs at most
1000 deep (SQLite stops views there; it allows longer CTE chains).

## How it runs

`prepare` compiles the statement into closures (no bytecode interpreter): each FROM item
becomes a loop over a b-tree cursor (inner joins ordered so that each table is read through its
cheapest path given the ones before it), chosen per table from the WHERE and ON terms: a rowid
lookup or range, an index with equalities on its leading columns and a range on the next, a
covering index, or a full scan. An ORDER BY that an index (or the rowid) already gives needs
no sort, and LIMIT then stops the loops early; an index giving its first terms is scanned too,
the sort then ordering only rows equal in those. Columns are decoded from the record only when
read, and the table row behind an index entry is found only when a column the index lacks is
read. A correlated subquery runs again for each outer row; an uncorrelated one once per run.
