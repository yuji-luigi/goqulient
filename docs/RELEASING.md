# Releasing

One version for the whole repo: the extension ships the engine inside its VSIX, so `vscode/package.json` and the
engine's Cargo workspace always carry the same version, and a release is a `vX.Y.Z` tag. The maintainer's side (trying
a VSIX in each editor, store accounts, trusted publishing, troubleshooting) is in [PUBLISHING.md](PUBLISHING.md).

## Flow
1. Commit with [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `feat!:` …).
2. On every push to `main`, `.github/workflows/release.yml` runs release-please (`release-please-config.json`, one root
   package, release type `simple`, `bump-minor-pre-major`). It keeps a release PR open that updates `CHANGELOG.md` and
   bumps the version in:
   - `vscode/package.json` and `vscode/package-lock.json` (`$.version`, `$.packages[''].version`);
   - `engine/Cargo.toml` (`$.workspace.package.version`) and the two workspace crates in `engine/Cargo.lock`, so
     `cargo build --locked` keeps working on the tag.
3. Merging the release PR tags `vX.Y.Z`, creates the GitHub release, and `release.yml` calls
   `extension-release.yml` (`workflow_call`) on that tag, then publishes its VSIXs in its own `publish` job. A tag
   pushed with `GITHUB_TOKEN` wouldn't start a workflow on its own, hence the call.

Run **Extension release** by hand (Actions → Extension release → Run workflow) to get the VSIXs as a workflow artifact
without publishing. Run **Release** by hand with a `tag` (and `prerelease`) to build and publish an existing release
again, e.g. after a failed publish.

## `extension-release.yml` (build and package, never publishes)
- **engine**: five jobs for six targets, each uploading `engines-<job>` with one `<target>/` folder per binary. Linux:
  static musl via `cargo-zigbuild` (x64 + arm64), so any distro and Alpine work. macOS: **one** arm64 runner builds
  arm64 and cross-compiles x64, because hosted macOS runners are scarce (a second macOS job was once cancelled for lack
  of capacity). Windows: x64 + arm64 MSVC. The binaries native to their runner run `--version`.
- **package** runs even when an engine job failed or was cancelled (`if: always()`): it merges the artifacts into
  `engines/`, writes a table of built and missing targets to the job summary, fails only if no engine was built, and
  exposes the missing targets as the workflow output `missing`. Then typecheck, production build and
  `scripts/package-vsix.sh`: one `vsce package --target <target>` per binary, with the binary in `vscode/bin/<target>/`,
  → `dist/goqulient-<target>.vsix` (artifact `vsix`). `vscode:prepublish` copies the root `LICENSE` into `vscode/` so
  the package carries it. vsce ignores `repository.directory`, so the script passes `--baseContentUrl` and
  `--baseImagesUrl` to point the README's relative links and images at `vscode/` on GitHub. The extension restores a
  lost exec bit before spawning the engine.
- Input `prerelease` (default true) adds `--pre-release` to `vsce package`.
- `scripts/dev.sh vscode package <engines dir>` runs the same script locally on engines downloaded from a run. It
  packages a copy on the container's own disk: in a folder shared from a Mac, the case-insensitive disk makes the Linux
  side list `README.md` twice after the first package, and vsce refuses "the same case insensitive path".

## `release.yml`: publish
- The `publish` job is in `release.yml`, not in the called workflow, because the Marketplace's trusted publishing
  policy names one workflow file. In a called workflow, GitHub's OIDC token names both the caller (`workflow_ref`) and
  the called file (`job_workflow_ref`); in `release.yml` both are `release.yml`.
- **All or nothing**: it stops before uploading anything if `extension-release.yml` reports a missing target or the
  `vsix` artifact doesn't hold six files.
- Marketplace: `npx @vscode/vsce@4 publish --oidc --packagePath vsix/*.vsix` with `permissions: id-token: write`
  (trusted publishing; `--oidc` is in vsce 4.1.0 and later). There is no `VSCE_PAT` any more: Azure DevOps retires
  global personal access tokens.
- Open VSX: `ovsx publish` per VSIX with the `OVSX_PAT` secret; without it, the step only warns.
- Both use `--skip-duplicate`, so a re-run publishes only what's missing. `--pre-release` is passed on push (until
  1.0) and when the manual run's `prerelease` input is on.

## Before the first publish
The full list is in [PUBLISHING.md](PUBLISHING.md#6-checklist-before-the-first-public-release): the Marketplace
publisher `yuji-luigi` and its trusted publishing policy for `release.yml`, the Open VSX namespace and `OVSX_PAT`, the
repository setting that lets release-please open pull requests, and a public repository. release-please computes the
first version from the whole history (there is no earlier tag); to pin it, set `"release-as": "X.Y.Z"` on the root
package for that one release PR, then remove it.

## Notes
- macOS and Windows binaries are not signed (macOS: not notarized either). VS Code and Cursor extract the VSIX
  themselves, so no quarantine flag is set; see PUBLISHING.md's troubleshooting for Defender and Smart App Control.
- The `Cargo.lock` entries are matched with `$.package[?(@.name.value==='goku-engine')]`: release-please's TOML parser
  wraps every value as `{value, start, end}`. If a release PR stops bumping `Cargo.lock`, check that expression first.
- Name: **GoQulient** (pronounced "go-kyu-lient"): package `goqulient`, display name `GoQulient`, every contributed id
  (settings, commands, view container `goqulient`, view `goqulient.connections`, webview view types, context values)
  under `goqulient.*`, SecretStorage `goqulient.password.<id>`, output channel "GoQulient", ER layout keys
  `goqulient.er:…`. Marketplace extension ids are permanent. Internal names stay as they are: the engine crates
  (`goku-engine`, `goku-engine-server`) and the `GOKU_*` environment variables of the dev and test tooling.
