# Parity fixtures

JSON snapshots of GokuCore behaviour (the engine of the Goku macOS app, written in Swift) that the Rust engine
(`engine/`, see [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md)) must reproduce. The Rust tests that read them are
`engine/crates/goku-engine/tests/parity_{schema,mongo_inference,er_layout,query}.rs`.

**Generated, never edited by hand.** The files are written and checked by the app's test suite
(`ParityFixtureTests.swift` and `ParityQueryFixtureTests.swift` in the Goku app's repository): every app test run compares its output with them, so a
GokuCore change that alters them fails there until they are regenerated. Updates reach this repository as a **sync pull
request from the app's repository** that replaces the changed files (and the seeds in `docker/*/init/` when those
changed too), so both engines move together. If a Rust change breaks a parity test here, fix the Rust side, or open an
issue if you think GokuCore is wrong; don't change the JSON.

```bash
scripts/dev.sh up && scripts/dev.sh engine test   # includes the parity tests
```
`er-layout`, `sql-split`, `sql-safety`, `danger` and `mongo-shell` need no database; `schema` and `mongo-inference`
need the Docker services and are skipped without `GOKU_IT=1` (set in the `engine` container). The Rust tests iterate every `*.json` in each directory, so a sync PR also deletes stale files.

## Files

| Path | What | Database |
|---|---|---|
| `er-layout/<case>.json` | `ERLayout.layout` on fixed inputs: `empty`, `single`, `chain` (plus ignored self-loop, duplicate edge, unknown parent, entity without size), `star-multi-parent` (multi-parent, diamond, cycle), `disconnected-grid` (two components, singleton grid, custom options, fractional sizes), `large-30` (30 tables from a fixed LCG) | none |
| `mongo-inference/<case>.json` | `MongoSession.introspect` on scratch databases filled with the given documents: `references` (DBRef, name match, value probe, numeric/string refs, array refs, `parent` self-reference, refs that must *not* match), `types` (every common BSON type, mixed types, nulls, depth limit, 20-element array limit, unique/compound indexes, empty collection) | `mongo` |
| `schema/postgres.json` | `introspect(database: "shop", schemas: ["public", "inventory"])` on the seed plus four `parity_*` tables for 1:1 (FK = PK, single unique column, composite unique in another column order) | `pg-primary` |
| `schema/postgres-inventory.json` | `schemas: ["inventory"]` only: the FK into `public` becomes a warning | `pg-primary` |
| `schema/mysql.json` | `introspect(database: "shop")` on the seed plus the same `parity_*` tables | `mysql-primary` |
| `schema/mysql-analytics.json` | `analytics`: cross-database FK → warning, no relationship | `mysql-primary` |
| `schema/mongodb.json` | inference on the seeded `shop` database | `mongo` |
| `sql-split/{postgres,mysql}.json` | `SQLSplitter.splitTrackingDatabase(script, database: "shop")` per script: quotes, doubled quotes, backslash and `E''` escapes, nested comments, dollar quoting, `DELIMITER`, `/*! */`, `#` and MySQL `--`, `USE` tracking, CRLF, unterminated strings and comments, non-ASCII | none |
| `sql-safety/{postgres,mysql}.json` | per statement: `NormalizedSQL` (text, executable comment, leading keyword), `isPotentiallyWriting`, `returnsRows`, `useTarget`, and `checkSQL` with `readOnly` on and with only the `rules` | none |
| `danger/{postgres,mysql,mongodb}.json` | `DangerCheck.findings(for:kind:database: "shop")` per script (CTEs with writes, `EXPLAIN ANALYZE`, `DO`/`CALL`/`EXECUTE`, executable comments, `USE`/`use`, `$out`/`$merge`, untargeted Mongo writes, unparseable scripts) | none |
| `mongo-shell/values.json` | `MongoShellParser.parseValue` per text: relaxed JSON, numbers, strings and escapes, regex literals, constructors, Extended JSON wrappers, duplicate keys, errors with line and column | none |
| `mongo-shell/statements.json` | `MongoShellParser.parseScript(script, database: "shop")` per script: each statement's text, database (after `use`), `MongoCall`, `isWrite` and `writeTargets`, or the parse error | none |

The database outputs depend on the Docker image versions (PostgreSQL 17, MySQL 8.4, MongoDB 7): type spellings such as
`character varying(20)` or MySQL's `datetime DEFAULT_GENERATED` come from the server.

## Shapes

Every file is `{ "input": …, "output": … }`. Encoding: `JSONEncoder` with pretty printing and sorted keys, trailing
newline. Compare **parsed values**, not text: Swift writes `90` where serde writes `90.0`, and the whitespace is Swift's.
Numbers are what Swift's encoder prints for a `Double` (shortest round-trip form), so parse them as `f64` and compare
exactly. Optional fields that are `nil` are **absent** (Swift's synthesized `encodeIfPresent`), never `null`.

### `er-layout`
```
input:  { entities: [String],                       // order matters (it is the layout's input order)
          sizes: { id: { width, height } },          // ERSize
          edges: [ { child, parent } ],
          options: { horizontalGap, verticalGap, componentGap, orderingSweeps } }   // ERLayout.Options
output: { id: { origin: { x, y }, size: { width, height } } }                       // ERRect
```
Ids are ASCII, so Swift's `String <` (used to break barycenter ties) equals byte order. Swift's `sort` is stable, and
the component packing relies on that for components of equal area (Rust's `sort_by` is stable too).

### `mongo-inference`
```
input:  { database,                                  // scratch database, dropped before and after
          collections: [ { name,
                           documents: [String],      // one canonical Extended JSON v2 document per string
                           indexes: [ { name, fields: [String], unique } ] } ] }   // all keys ascending (1)
output: SchemaModel
```
Documents are strings so field order and BSON types survive exactly; parse them with key order preserved (e.g. the
`bson` crate's Extended JSON parser, or serde_json with `preserve_order`). Insert each collection in order (an empty
list means `create` it), then create its indexes, then introspect the whole database.

### `schema`
```
input:  { database, schemas: [String],
          setup: [String], teardown: [String],       // SQL run in `database` before / after introspecting
          entities: [String] }                       // ids kept in output (see Normalization)
output: SchemaModel
```

### `sql-split`, `sql-safety`, `danger`
```
sql-split:  input  { dialect: "postgres"|"mysql", database, scripts: [String] }
            output [ { statements: [String], databases: [String] } ]          // one per script; databases follow USE
sql-safety: input  { dialect, database, rules: [ { database, table } ], statements: [String] }
            output [ { normalized, executableComment, keyword, writes, returnsRows, useTarget?,
                       readOnlyRefusal?, rulesRefusal? } ]                     // refusals: DBError.displayMessage
danger:     input  { kind: "postgres"|"mysql"|"mongodb", database, scripts: [String] }
            output [ [ { warning, action, object?, database, statement } ] ]   // DangerFinding per script
```

### `mongo-shell`
```
values:     input  { values: [String] }
            output [ { value? | error? } ]       // value: canonical Extended JSON (ExtendedJSON.string) of the parsed value
statements: input  { database, scripts: [String] }
            output [ { statements?: [ { text, database, call, isWrite, writeTargets?: [ { database, collection? } ] } ],
                       error? } ]                // error: the parse error's displayMessage ("Parse error: … at line l, column c")
call:       { kind: "showDatabases"|"showCollections"|"getName"|"use"|"command"|"adminCommand"|"find"|"aggregate"|"count"
                    |"insert"|"update"|"delete"|"renameCollection",
              name?, collection?, document?, filter?, projection?, sort?, skip?, limit?, explain?, pipeline?, documents?,
              update?, multi?, upsert?, from?, to?, dropTarget? }
```
Documents in `call` are canonical Extended JSON strings, so key order and BSON types are exact; compare them as ordered
documents. `writeTargets` absent = the target can't be told (`nil`), `[]` = no target. A `command` whose argument was an
array (`db.runCommand([1, 2])`) holds a BSON array.

### `SchemaModel` (Swift `Codable`, `SchemaModel.swift` in the Goku app)
```
{ database, engine: "postgres"|"mysql"|"mongodb", warnings: [String],
  entities: [ { id, schema, name, kind: "table"|"view"|"materializedView"|"collection",
                comment?, attributes: [Attribute], indexes: [ { name, columns: [String], unique, primary } ] } ],
  relationships: [ { id, name, fromEntity, fromColumns, toEntity, toColumns,
                     cardinality: "manyToOne"|"oneToOne", optional, onDelete?, onUpdate?, inferred } ] }
Attribute: { name, type, nullable, isPrimaryKey, isForeignKey, isUnique, defaultValue?, comment?, presence? }
```
`Entity.id` is `schema.name` (or `name` when the schema is empty); `Relationship.id` is
`from(cols,…)->to(cols,…)#name`. `Entity.rowEstimate` exists in Swift but is never in the fixtures (below).

## Normalization (applied to `output` before writing / comparing; Rust must apply the same)
- `rowEstimate` is removed from every entity (PG `reltuples`, MySQL `TABLE_ROWS`, Mongo `count`).
- **Database fixtures keep only `input.entities`**: other suites leave scratch tables/collections (`it_*`, `dump_*`) in the
  same databases. Relationships are kept when both ends are kept; warnings that contain ` references ` are kept only when
  they start with `<kept id>.`. Entity order is otherwise the server's (`ORDER BY` in the introspection SQL, collation
  included), relationship order is `finalizeRelationships`' `(fromEntity, name)`.
- **MongoDB attributes are sorted by name** (byte order). GokuCore lists them in first-seen order of the `$sample`d
  documents, and `$sample` returns documents in random order, so the original order is not reproducible.
- Nothing else is volatile here: ObjectIds in the inference inputs are fixed, the seeded Mongo `shop` ObjectIds and dates
  only show up as types, and no OIDs, versions or timestamps are part of `SchemaModel`.
