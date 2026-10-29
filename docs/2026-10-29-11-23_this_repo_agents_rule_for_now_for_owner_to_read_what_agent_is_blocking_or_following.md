# Agent rules in this repository: what a coding agent follows, and what gets blocked

A snapshot for the owner, written on 2026-10-29 after the first release (v0.1.0). It explains what a coding agent such
as Claude Code does on its own in `goqulient`, what it asks you about first, and what gets blocked, and why each rule
exists. The source of truth stays [AGENTS.md](../AGENTS.md) (loaded by every agent through `CLAUDE.md`); this file is
the explanation behind it.

## 1. Rules the agent follows (from AGENTS.md)

| # | Rule | What the agent does | Why |
|---|---|---|---|
| 1 | **Docker only** | Runs cargo, npm and every test through `scripts/dev.sh` inside the `goqulient` compose project. Never installs Rust, Node or other toolchains on your Mac. | See section 2: a runaway build or test can't hang or crash the Mac, and nothing pollutes it. |
| 2 | **Test databases are shared and throwaway** | Tests create scratch objects with random suffixes (`rs_pg_*`, `rs_my_*`, `rs_mg_*`) and drop them; never two test runs at once; never a real database. | A test can't damage real data, and parallel runs can't trip over each other. |
| 3 | **Safety is a product requirement** | Keeps deadlines on every network wait, row caps, read-only enforced twice, secrets only in SecretStorage. Never weakens these to make a feature or a test easier. | The extension's promise to users: don't hang or crash their machine, don't hurt their database, don't leak secrets. |
| 4 | **Profiles decode tolerantly** | New connection fields get defaults; unknown values fall back safely; unreadable entries stay in the list. | Saved connections must survive upgrades. |
| 5 | **Parity fixtures are generated, not written** | Never edits `parity/` by hand. If a parity test fails, the Rust side is wrong. | The fixtures come from the Goku app's test suite; they are the contract between the two engines. |
| 6 | **Every behaviour change comes with a test** | Adds the matching test (Rust, protocol, driver, Node or end-to-end); for webview changes, takes and looks at screenshots. | Changes stay verifiable. |
| 7 | **Match the surrounding code** | Dense code, few comments explaining *why*, `cargo fmt` at width 120, user-facing errors through `DbError`. | Consistency. |
| 8 | **Conventional Commits** | `feat:`, `fix:`, `ci:`, `docs:`, `feat!:` … | release-please computes the version and changelog from them. |
| 9 | **Docs in the same change** | Updates ARCHITECTURE, TESTING, RELEASING or PUBLISHING when behaviour, the protocol or a file's role changes. | Docs don't drift from the code. |

## 2. Why development and tests run in Docker

1. **A runaway process can't take the Mac down.** `docker-compose.yml` caps every container:

   | Container | Memory | CPUs | Processes |
   |---|---|---|---|
   | `engine` (Rust) | 5 GB | 4 (2 in CI) | 4096 |
   | `vscode` (Node) | 2 GB | 2 | 512 |
   | `vscode-e2e` (VS Code under xvfb) | 3 GB | 2 | 512 |
   | databases | PostgreSQL 384 MB, MySQL 640 MB, MongoDB 512 MB | 1 each | 512 |

   `scripts/dev.sh` also kills a test run after 900 s (`timeout -s KILL`). A memory leak, an endless loop or a
   process explosion is stopped inside Docker; the Mac stays responsive.
2. **The databases are throwaway and walled off.** They are seeded from `docker/*/init`, listen only on `127.0.0.1`
   on unusual ports (25432, 23306, 27217), and `scripts/dev.sh down` deletes them with their volumes.
3. **Nothing is installed on the Mac.** Linux build output and `node_modules` live in Docker volumes
   (`engine-target`, `vscode-node-modules`), not in your checkout.
4. **Same results as CI.** Pinned images (`rust:1.99.1-bookworm`, `node:24-bookworm`); CI runs the same compose setup.
5. **Separation from your files and secrets.** Containers see the repository folder only: not your home directory,
   keychain or SSH keys. A malicious package or build script runs in a box.

Known trade-off: the repository is shared from the Mac's case-insensitive disk. Packaging a VSIX in that shared folder
fails from the second package on ("same case insensitive path", README.md seen twice).
`scripts/dev.sh vscode package` packages a copy on the container's own disk instead.

## 3. What the agent asks you before doing

These are hard to undo or visible to others, so the agent only does them when you asked for that specific action in
this conversation. An earlier "yes" doesn't carry over to a different action.

| Action | Why it waits for you |
|---|---|
| Committing, pushing, opening a PR | Your repository, your history. When on `main`, the agent branches first. |
| **Merging a PR**, especially into `main` | A merge to `main` triggers release-please; merging the release PR publishes. |
| **Publishing** (merging the release PR, approving the `marketplace` deployment) | Irreversible: the version number and the extension ID are permanent. |
| Changing repository settings, secrets, environments | Security and release gates. Today you made these changes yourself; the agent only read them through the API. |
| Deleting or overwriting things (files, branches, releases) | The agent checks the target first, and deletes only what it created and what's merged. |
| Creating accounts or tokens | Never done by the agent. |

## 4. What got blocked today, and why

Claude Code's permission system ("auto mode") checks risky commands before they run, independently of the agent's own
judgement. Blocks seen in this session:

| What the agent tried | Blocked by | Why | What happened instead |
|---|---|---|---|
| A `docker run … bash -c '… rm -rf …'` script to simulate CI packaging | The built-in removal safety check | It couldn't verify what `rm -rf` inside a `bash -c` string would delete. | Rewritten without the removal (a fresh `git clone` doesn't need cleaning). |
| One command that merged PR #3, ran `git branch -D` and polled the run | The auto-mode classifier | Force-deleting a branch plus merging in one compound command. | The merge ran alone; the branch was deleted with the safe `git branch -d`. |
| Merging PR #4 before you had said so for that PR | The auto-mode classifier ("Merge Without Review") | You had approved earlier PRs, not this one. | The agent stopped and asked; it merged after you said "accept the #4 PR". |

To allow a kind of action permanently, add a permission rule in `.claude/settings.json` (Claude Code's `/permissions`
command, or the `update-config` skill). Keep merges and publishing manual.

## 5. The release gates (in place since v0.1.0)

| Gate | Where | Effect |
|---|---|---|
| Release PR | GitHub, PR "chore(main): release X.Y.Z" | Nothing is released until you merge it. |
| Environment `marketplace` | GitHub → Settings → Environments | Only `main` may deploy; **you approve every publish** (Review deployments → Approve and deploy). |
| Trusted publishing policy | Marketplace → publisher `yuji-luigi` → Trusted Publishing | Accepts only `yuji-luigi/goqulient`, `release.yml`, environment `marketplace`. No token exists to leak. |
| All-or-nothing check | `release.yml`, `publish` job | Publishes only when all six platform VSIXs exist. |

What remains is your GitHub account and the Microsoft account that owns the Marketplace publisher: keep two-factor
authentication on both, ideally with a passkey or security key. The GitHub-side gates are also kept as Terraform in the
separate project `vscode_extension_marketplace_github`; the Marketplace policy can only be set by hand. Details:
[PUBLISHING.md](PUBLISHING.md).

## 6. Rules that applied only to this session's task

The work came as a brief from a coordinating "PM" agent session, with its own limits for that task: don't push, don't
run workflows on GitHub, don't change repository settings, don't publish, don't create accounts or tokens. Reading
with `gh` (runs, artifacts, logs) was allowed. You lifted those limits step by step in this conversation (push, open
PRs, merge, release, approve). They are not standing rules of the repository.
