---
name: rust-engine
description: How to write and review Rust in GoQulient's database engine (engine/, the goku-engine library and the goku-engine-server binary). Use for any change to a .rs file, engine/Cargo.toml or engine tests, including new drivers, protocol methods, SSH tunnels and query execution, and when reviewing such a change.
---

# Writing Rust in the GoQulient engine

`engine/` is a Cargo workspace: `goku-engine` (library: drivers, safety primitives, introspection, Mongo inference, ER
layout) and `goku-engine-server` (stdio JSON-RPC binary the VS Code extension spawns). The library will also be linked
by the Goku Mac app through uniffi (milestone M4), so it is a product library, not glue for VS Code. Read `AGENTS.md`
first; this skill condenses how to write the code. When the two disagree, `AGENTS.md` wins.

## 1. The engine's invariants (never trade these for convenience)
These are the product's safety promise: don't hang or crash the user's machine, don't hurt the database, don't leak
secrets. A change that weakens one is wrong, even if the tests pass.

- **Every network wait has a deadline.** Wrap it in `deadline::with_deadline(seconds, what, cancel, on_timeout, fut)`.
  `on_timeout` must stop the work on the other side: close the socket, send `CancelToken::cancel_query`
  (PostgreSQL), `KILL QUERY` on a second connection (MySQL), or kill the operation (MongoDB). Dropping a future is not
  enough: a spawned connection task or a server statement outlives it. Set the server-side limit too
  (`statement_timeout`, `max_execution_time`, `maxTimeMS`). Connect uses `connect_timeout_seconds`, everything else
  `query_timeout_seconds`. New I/O (an SSH handshake, a port forward, a query stream) is no exception.
- **Rows are streamed and capped.** Results go through `limits::RowCollector` (profile `max_rows`, default 1,000, hard
  cap `HARD_MAX_ROWS` = 100,000) and cells through `limits::clip`. Never `collect()` a whole result set or a whole
  cursor. When the cap is hit, stop the server from streaming the rest (cancel, drop the connection) and mark the
  result truncated.
- **Read-only is enforced twice:** the client check *and* the server session (`default_transaction_read_only`,
  `SET SESSION TRANSACTION READ ONLY`). MongoDB has no server-side read-only, so its client check stays strict.
  A new execution path calls both; it never relies on one.
- **Destructive statements need confirmation** (`needsConfirmation`, the Goku app's statement list) and a grid edit
  hits exactly one row or the batch rolls back.
- **Secrets never leave memory unmasked.** Not in logs, argv, environment variables, error messages, `Debug` output,
  endpoint summaries or tree labels. Run user-facing text through `redact::secrets`, URIs through `redact::uri`
  (`redact.rs` and `vscode/src/connections/redact.ts` share the rules). A type that holds a secret gets a hand-written
  masked `Debug` (see `Credentials`), never `#[derive(Debug)]`. Secrets arrive over stdin only.
- **One tokio runtime per process**, built by the server. The library never builds a runtime, never calls
  `block_on`, and never prints: stdout carries only protocol frames, logs go to stderr from the server.
- **Parity fixtures are generated, never edited.** `parity/` comes from the Goku app's test suite. If a parity test
  fails, the Rust side is wrong (or the app must change first). Don't touch the JSON.
- **Profiles decode tolerantly** (`profile.rs`): a new field gets a default in `from_fields` and a test; unknown enum
  values fall back to the safe case (unknown SSL mode = strictest); the JSON shape matches the Goku app's.

## 2. Errors
- Library errors are `DbError` (`error.rs`, `thiserror`, same wording as GokuCore's `DBError`). Users see
  `display_message()` (secrets removed, capped). Pick the closest existing variant before adding one; a new variant
  needs a `kind` the extension understands (`vscode/src/engine/client.ts`).
- No `anyhow` in `goku-engine` (typed errors only, the app must be able to match on them). Errors that reach the
  protocol go through `DbError` (`-32001`, `data = {kind, detail}`); the server adds only the JSON-RPC codes.
- Convert driver errors once, at the driver boundary, keeping the server's message (it is what the user needs) but
  passing it through redaction. Don't `map_err(|_| …)` away the cause.
- **No panics on reachable paths.** No `unwrap`, `expect`, `panic!`, `unreachable!` or slice indexing on anything a
  server response, a user, a profile or a file can influence: rows with fewer columns than expected, empty result
  sets, odd type OIDs, malformed BSON, truncated frames. Use `get()`, `let … else`, `?`, `ok_or_else`. A panic in a
  request task kills that request at best and the engine process at worst.
- Panics are for bugs you have *proven* impossible (a regex literal that compiles, an invariant of your own data
  structure). Write `expect("why this can't fail")`, and add `#[allow(clippy::expect_used, reason = "…")]` if the
  lint fires. Lock poisoning: `lock().unwrap_or_else(PoisonError::into_inner)` or treat it as a bug.
- Test the error path, not just the happy one: timeouts, cancellation, auth failure, read-only refusal.

## 3. Async
- **Never block the runtime.** No `std::thread::sleep`, blocking file or DNS I/O, or CPU-heavy loops (big ER layouts,
  inference over many documents) in an async fn without `tokio::task::spawn_blocking` or yield points
  (`tokio::task::yield_now`). A blocked worker stalls every other request, including `$/cancelRequest`.
- **Every async path takes a `&CancellationToken`** and stops promptly when it fires. Check it before starting work
  and pass it down; `with_deadline` already races it.
- **Cancellation safety.** Any `.await` can be the last line that runs: the future may be dropped there. Don't leave
  shared state half-updated across an await (a pooled connection marked busy, a session map entry without a
  session). When a cancel or timeout interrupts a stateful conversation (a PostgreSQL connection mid-query, an SSH
  channel), drop that connection rather than reuse it: a late cancel could hit the next statement.
- **`select!` pitfalls.** Branches that lose are dropped: only put cancellation-safe futures there, or pin a future
  outside the loop and select on `&mut fut`. Use `biased;` when order matters (a ready result should win over an
  expired timer, as in `with_deadline`). Don't create a fresh `sleep` inside a looping `select!` if it is meant as an
  overall deadline.
- **No lock held across `.await`.** `std::sync::Mutex` guards end before any await (clippy `await_holding_lock`).
  Use `tokio::sync::Mutex` only when the guard really must live across an await, as in `Conn::lock`, which serialises
  one request per connection by design; keep such sections short and bounded by a deadline.
- Spawned tasks: keep the `JoinHandle` or a `CancellationToken` so the task can be stopped; a detached task that
  holds a socket must end when its session closes. Bounded channels for anything a peer can flood.
- Futures and public types are `Send` (they run on a multi-thread runtime and cross to Swift later).

## 4. API shape for M4 (uniffi)
The Mac app will call the library through a thin FFI crate (uniffi), which only translates; business logic stays in
`goku-engine` as idiomatic, safe Rust. Make that translation cheap:
- **Owned data at the boundary.** Public functions take and return owned `String`, `Vec<T>`, `Option<T>`, plain
  structs and enums. Internal helpers may borrow freely.
- **No lifetimes or generics in public types**, and no `impl Trait` or closures in public signatures that the app would
  need. `with_deadline` is generic because it is an internal building block; keep generics like it out of the
  session-level API.
- **Plain enums and structs** with `serde` derives in the JSON shape the protocol uses (camelCase,
  `#[serde(tag = …)]`). Fieldless or simple-payload enums map well to Swift; avoid deeply nested generic wrappers.
- **Don't leak third-party types** (`tokio_postgres::Row`, `bson::Document`, `mysql_async::Value`) out of the
  driver modules. Convert to `model.rs` types.
- **Nothing VS Code-specific in `goku-engine`:** no JSON-RPC, no framing, no `vscode` names, no settings keys, no
  assumptions about webviews. Protocol shapes and method names live in `goku-engine-server`.
- Sessions are `Box<dyn DatabaseSession>` behind `session::connect`; add capabilities as trait methods that every
  driver implements (or answers with `DbError::Unsupported`), not as downcasts.

## 5. Ownership, allocation, iterators
- Borrow (`&str`, `&[T]`) in internal functions; clone only when you need ownership or a snapshot. A `clone()` in a
  per-row or per-cell loop deserves a second look.
- Pass small `Copy` types by value; give the collection its capacity when you know it (`Vec::with_capacity`).
- Prefer the lazy `_else` variants (`ok_or_else`, `unwrap_or_else`, `map_or_else`) when the fallback allocates or
  formats.
- Iterator chains for transformations (`map`/`filter`/`collect`); a `for` loop when there are early exits, `?`,
  `await`, or several side effects. Don't `collect()` just to iterate again. Long unreadable chains get split up.
- Cell formatting runs per cell over up to 100,000 rows: avoid allocating more than the output string.

## 6. Tests (AGENTS.md rule 6: every behaviour change comes with one)
| What changed | Where the test goes |
|---|---|
| Pure logic (formatting, parsing, layout, redaction, limits) | `#[cfg(test)] mod tests` next to the code |
| A protocol method, framing, dispatch, cancellation, error codes | `goku-engine-server/src/tests.rs` (in-memory pipe) |
| A driver against a real database | `goku-engine/tests/{postgres,mysql,mongo}.rs` |
| GokuCore parity | `tests/parity_*.rs`, comparing parsed values against `parity/` (never editing it) |

- Driver tests: get a profile from `support::profile(DatabaseKind::…)`, wrap the body in `support::run(seconds, …)`
  (a hard deadline), and return early unless `support::enabled()` (`GOKU_IT=1`, set in the `engine` container).
- Scratch objects get random suffixes (`rs_pg_<random>`, `rs_my_<random>`, `rs_mg_<random>`; `support::mongo::scratch`)
  and are dropped at the end. The test databases are shared: never depend on another test's objects, never modify
  the seeded ones.
- Name tests as the behaviour they check (`timeout_cancels_the_server_query`), one behaviour per test. Assert the
  ground truth (the server really is read-only), not your own implementation echoed back.
- Timing-dependent tests use `tokio::time::pause()` (the `test-util` feature) rather than real sleeps.
- `unwrap`/`expect`/indexing are fine in tests (`clippy.toml` allows them there).

## 7. Docs and comments
- Every public item in `goku-engine` gets a `///` doc comment: what it does, its limits (deadline, cap), when it errors.
  First sentence on one line. Modules open with `//!` saying what they own and which invariants apply.
- Comments explain *why*: a server quirk, an invariant, a GokuCore behaviour being mirrored. Don't narrate the code.
- "Port of GokuCore's `X`" names the Swift file (`PostgresSession.swift`) as plain text, never a link.
- Magic numbers (caps, timeouts, OIDs) get a named constant or a comment saying where they come from.
- A TODO names an issue or a milestone, or it doesn't go in.
- Behaviour, protocol or file-role change → update `docs/ARCHITECTURE.md` (file map, protocol table, checklists) in
  the same commit.

## 8. Before you finish
Everything runs in Docker (never install cargo on the host):
```bash
scripts/dev.sh engine check          # cargo fmt --check + clippy --all-targets -D warnings (workspace lints)
scripts/dev.sh engine test [filter]  # unit, protocol, integration (GOKU_IT=1) and parity tests
scripts/dev.sh vscode test           # when the protocol or a result shape changed: the extension's tests
```
`scripts/dev.sh up` first if the databases aren't running. Don't run two test runs against the same compose project
at once; a git worktree gets its own project (docs/TESTING.md, "Parallel worktrees"). Fix lint warnings rather than
silencing them; a local `#[allow(clippy::…, reason = "…")]` is for a justified exception, never a whole module.
Commit with Conventional Commits (`feat(engine): …`, `fix(engine): …`).

## 9. Review checklist
Severity: **P0** must not merge, **P1** fix before merge, **P2** fix or justify, **P3** nice to have.

- **P0** A network wait without `with_deadline`, or an `on_timeout` that doesn't stop the server-side work.
- **P0** An unbounded result: `collect()` of rows or a cursor, no `RowCollector`, no cell clip.
- **P0** A write path that skips the client read-only check or the server read-only session.
- **P0** A secret in a log, an error message, `Debug` output, argv or env; a `#[derive(Debug)]` on a type with one.
- **P0** A panic (`unwrap`, `expect`, indexing) on data from a server, a user or a file.
- **P0** A runtime built, `block_on` or stdout output in the library; an edited parity fixture.
- **P1** A blocking call on the runtime; a lock held across `.await`; a cancel or timeout that leaves a connection
  in use.
- **P1** A swallowed error (`let _ =`, `.ok()`, `map_err(|_| …)`) that hides why something failed.
- **P1** A new profile field without a default in `from_fields` and a test; a JSON shape that differs from the app.
- **P1** A behaviour change without a test, or a test that can't fail.
- **P2** A third-party type, lifetime or generic in the public API; VS Code specifics in `goku-engine`.
- **P2** A public item without a doc comment; docs (`ARCHITECTURE.md`) not updated.
- **P2** A flaky test (real sleeps, shared scratch names, order dependence).
- **P3** Avoidable clones or allocations in per-row code; comments that restate the code; unreadable iterator chains.

## Sources
Condensed and adapted for this engine from:
- [Pragmatic Rust Guidelines](https://github.com/microsoft/rust-guidelines), Microsoft, MIT License: M-PANIC-IS-STOP,
  M-PANIC-ON-BUG, M-FFI-TRANSLATES, M-DONT-LEAK-TYPES, M-TYPES-SEND, M-YIELD-POINTS, M-STATIC-VERIFICATION,
  M-CANONICAL-DOCS, M-FIRST-DOC-SENTENCE, M-DOCUMENTED-MAGIC, M-LINT-OVERRIDE-EXPECT, M-TAUTOLOGICAL-TESTS.
- [Rust Best Practices](https://github.com/apollographql/rust-best-practices), Apollo GraphQL, Apache License 2.0:
  chapters on idioms, linting, errors, testing and documentation, and the severity matrix of its `rust-code-review`
  checklist.
