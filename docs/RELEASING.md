# Releasing

One version for the whole repo: the extension ships the engine inside its VSIX, so `vscode/package.json` and the
engine's Cargo workspace always carry the same version, and a release is a `vX.Y.Z` tag.

## Flow
1. Commit with [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `feat!:` …).
2. On every push to `main`, `.github/workflows/release.yml` runs release-please (`release-please-config.json`, one root
   package, release type `simple`, `bump-minor-pre-major`). It keeps a release PR open that updates `CHANGELOG.md` and
   bumps the version in:
   - `vscode/package.json` and `vscode/package-lock.json` (`$.version`, `$.packages[''].version`);
   - `engine/Cargo.toml` (`$.workspace.package.version`) and the two workspace crates in `engine/Cargo.lock`, so
     `cargo build --locked` keeps working on the tag.
3. Merging the release PR tags `vX.Y.Z`, creates the GitHub release, and `release.yml` calls
   `extension-release.yml` (`workflow_call`) on that tag. A tag pushed with `GITHUB_TOKEN` wouldn't start a workflow on
   its own, hence the call.

Run **Extension release** by hand (Actions → Extension release → Run workflow) to get the six VSIXs as a workflow
artifact without publishing.

## `extension-release.yml`
- **engine**: one job per target. Linux: static musl via `cargo-zigbuild` (x64 + arm64), so any distro and Alpine work;
  macOS: arm64 + x64; Windows: x64 + arm64 MSVC. The binaries native to their runner run `--version`.
- **package**: typecheck, production build, then one `vsce package --target <target>` per target with the binary in
  `vscode/bin/<target>/` → `goqulient-<target>.vsix`. `vscode:prepublish` copies the root `LICENSE` into `vscode/`
  so the package carries it. vsce ignores `repository.directory`, so `--baseContentUrl`/`--baseImagesUrl` point the
  README's relative links and images at `vscode/` on GitHub. The extension restores a lost exec bit before spawning
  the engine.
- **publish** (only when called with `publish: true`): `vsce publish` to the Marketplace with `VSCE_PAT`, `ovsx publish`
  to Open VSX with `OVSX_PAT`. Both secrets are optional: without one, that step only warns.
- Input `prerelease` (default true; `release.yml` passes true until 1.0) adds `--pre-release` to `vsce package`,
  `vsce publish` and `ovsx publish`.

## Before the first publish
- The Marketplace publisher is `yuji-luigi` (`vscode/package.json`): create it, or change the field.
- Add the `VSCE_PAT` and `OVSX_PAT` repository secrets (Open VSX also needs the namespace `yuji-luigi`).
- release-please computes the first version from the whole history (there is no earlier tag). To pin it, set
  `"release-as": "X.Y.Z"` on the root package for that one release PR, then remove it.

## Notes
- macOS binaries are not signed or notarized (VS Code extracts the VSIX itself, so no quarantine flag is set).
- The `Cargo.lock` entries are matched with `$.package[?(@.name.value==='goku-engine')]`: release-please's TOML parser
  wraps every value as `{value, start, end}`. If a release PR stops bumping `Cargo.lock`, check that expression first.
- Name: **GoQulient** (pronounced "go-kyu-lient"): package `goqulient`, display name `GoQulient`, every contributed id
  (settings, commands, view container `goqulient`, view `goqulient.connections`, webview view types, context values)
  under `goqulient.*`, SecretStorage `goqulient.password.<id>`, output channel "GoQulient", ER layout keys
  `goqulient.er:…`. Marketplace extension ids are permanent. Internal names stay as they are: the engine crates
  (`goku-engine`, `goku-engine-server`) and the `GOKU_*` environment variables of the dev and test tooling.
