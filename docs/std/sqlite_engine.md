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
- `Statement`, as in `velt:sqlite`: `all<T>(): T[]`, `get<T>(): T | null` (stops after the first
  row), `allWith<T>(params)`, `getWith<T>(params)`; also `columns(): string[]` and
  `values()` / `valuesWith(params)`, every value as a `Val` (`velt:sqlite/exec/value`),
  `columns().length` a row; `plan(): string[]`, how each FROM item is read, in the words of
  SQLite's `EXPLAIN QUERY PLAN` (`SEARCH posts USING INTEGER PRIMARY KEY (rowid=?)`).
- **Rows** fill `T` by column name through `velt:db`'s `readRows`, straight from the engine's
  values (no text in between): INTEGER into integer and `f64` fields, REAL into `f64`, TEXT into
  `string`, BLOB into `u8[]`, NULL into `T | null`. A row that doesn't fit throws
  `SQLITE_MISMATCH`.
- **Parameters**: `?`, `?NNN`, `:name`, `@name` and `$name`, bound as in `velt:sqlite`: an
  array (or a single value) by position, with one value for each parameter, an object by name,
  with a field for each; an integer in i64's range (and an integral `f64` in it) → INTEGER,
  other numbers → REAL, string → TEXT, bool → 0/1, null → NULL, an array of bytes → BLOB. A
  wrong number of values, a missing field, a `?` in a statement bound by an object, or no
  parameters at all for a statement that has some is `SQLITE_RANGE` with `velt:sqlite`'s
  message.
- `SqliteError { code, message }`: `SQLITE_ERROR` (unknown table, column or function, wrong
  arguments, an error while running such as `integer overflow` in `sum()`), `SQLITE_RANGE`,
  `SQLITE_MISMATCH`, `SQLITE_CORRUPT`, `SQLITE_CANTOPEN`. Messages are SQLite's
  (`no such column: x`, `misuse of aggregate: count()`).

```ts
import { open } from "velt:sqlite";
import { openDatabase } from "velt:sqlite/engine";

struct Post {
  id: i64;
  title: string;
  likes: i64;
}

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
  const top: Post[] = db
    .prepare(`SELECT p.id, p.title, (SELECT count(*) FROM likes l WHERE l.post_id = p.id) AS likes
              FROM posts p ORDER BY likes DESC, p.id LIMIT ?`)
    .allWith([10]);
  for (const p of top) {
    console.log(p.id, p.title, p.likes);
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
Window functions: `row_number`, `rank`, `dense_rank`, `percent_rank`, `cume_dist`, `ntile`,
`lag`, `lead`, `first_value`, `last_value`, `nth_value`, and the aggregates over a window, with
`PARTITION BY`, `ORDER BY`, `ROWS` and `GROUPS` frames, `RANGE` frames bounded by `UNBOUNDED` or
`CURRENT ROW`, `EXCLUDE`, `FILTER` and named windows (`WINDOW w AS (…)`). An aggregate over a
moving frame is computed as SQLite computes it, adding the rows that enter the frame and taking
out the ones that leave it, so its result is the same to the last bit (a `REAL` that passed
through the frame, the order of a floating-point sum).

Not yet (an error at `prepare`): statements other than SELECT, RIGHT and FULL joins, ORDER BY
inside a recursive CTE, `RANGE` frames with a `PRECEDING` or `FOLLOWING` offset, the `localtime`
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
the sort then ordering only rows equal in those. A query with window functions collects its
rows, sorts them by each window (as SQLite does, the loops give the first window's order when
they can), computes the functions and passes the rows on to DISTINCT, ORDER BY and LIMIT.
Columns are decoded from the record only when read, and the table row behind an index entry is
found only when a column the index lacks is read. A correlated subquery runs again for each
outer row; an uncorrelated one once per run.
