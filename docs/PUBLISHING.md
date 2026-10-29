# Publishing GoQulient

A guide for the maintainer: how to try a build in your editors, what to set up once, how a release gets to the stores,
and what to do when something goes wrong. The mechanics of the workflows (release-please, version bumps, jobs) are in
[RELEASING.md](RELEASING.md).

Facts about other products (editors, stores, Microsoft's token rules) were checked on 2026-10-29 against the sources
linked in each section. They change often: when something here disagrees with a store's own page, trust the store.

## What "publish everywhere" means

There are two stores, and every editor uses one of them:

| Editor | Gets extensions from |
|---|---|
| VS Code (Microsoft's build) | [Visual Studio Marketplace](https://marketplace.visualstudio.com/vscode) |
| Cursor | Open VSX, through Cursor's own mirror, which syncs periodically (since June 2025) |
| Windsurf, renamed Devin Desktop in June 2026 | Open VSX, through its own mirror |
| VSCodium | [Open VSX](https://open-vsx.org) |
| code-server (Coder) | Open VSX |
| Ona (formerly Gitpod), Eclipse Theia, Firebase Studio | Open VSX |

So publishing to the **Visual Studio Marketplace and Open VSX** reaches all of them.

Sources: [Cursor's announcement](https://forum.cursor.com/t/extension-marketplace-changes-transition-to-openvsx/109138)
(2025-06-25) and [the mirror's sync delay](https://forum.cursor.com/t/extension-not-searchable-stuck-on-old-version-in-cursor-marketplace/169510)
(2026-08-26); [Devin Desktop](https://docs.devin.ai/desktop/advanced);
[VSCodium](https://github.com/VSCodium/vscodium/blob/master/docs/extensions.md);
[code-server](https://github.com/coder/code-server/blob/main/docs/FAQ.md);
[Ona](https://ona.com/docs/references/ides-and-editors/vscode-extensions);
[Theia](https://theia-ide.org/docs/user_install_vscode_extensions/);
[Firebase Studio](https://firebase.google.com/docs/studio/customize-workspace).

## The release in one picture

```
merge the release PR ─► release.yml: release-please tags vX.Y.Z
                        ├─ extension-release.yml: engine × 6 targets ─► one VSIX per target (artifact "vsix")
                        └─ publish job (in release.yml): all 6 VSIXs or nothing
                             ├─ Visual Studio Marketplace: vsce publish --oidc  (trusted publishing, no secret)
                             └─ Open VSX: ovsx publish                          (OVSX_PAT secret)
```

## 1. Try a build before publishing

### Get the VSIX files
- **From GitHub:** Actions → **Extension release** → **Run workflow**. When it finishes, download the `vsix` artifact
  from the run page, or:
  ```bash
  gh run download <run-id> -R yuji-luigi/goqulient -n vsix -D dist/vsix
  ```
  If one engine job failed, the run still packages the other targets; the run's summary lists the missing ones.
- **On your Mac, from engines a run already built** (Docker only, no toolchain on the Mac):
  ```bash
  gh run download <run-id> -R yuji-luigi/goqulient -p 'engines-*' -D dist/engines
  scripts/dev.sh vscode package dist/engines --pre-release    # -> dist/goqulient-<target>.vsix
  ```

### Pick the right file
Each VSIX contains the engine for one platform. Installing the wrong one **succeeds without any warning**; the problem
only shows when the extension starts the engine (see [Troubleshooting](#5-troubleshooting)).

| Machine | How to tell | File |
|---|---|---|
| Mac with Apple silicon (M1 and later) | Apple menu → About This Mac shows "Chip: Apple M…"; `uname -m` prints `arm64` | `goqulient-darwin-arm64.vsix` |
| Mac with Intel | "Processor: Intel…"; `uname -m` prints `x86_64` | `goqulient-darwin-x64.vsix` |
| Windows on Intel/AMD | Settings → System → About → System type "x64-based processor" | `goqulient-win32-x64.vsix` |
| Windows on ARM (Snapdragon, Surface Pro X) | System type "ARM-based processor" | `goqulient-win32-arm64.vsix` |
| Linux, any distribution, Alpine included | `uname -m`: `x86_64` or `aarch64` | `goqulient-linux-x64.vsix` / `goqulient-linux-arm64.vsix` |

With Remote-SSH, WSL or a dev container, the extension runs on the **remote** side: pick the file for the remote
machine (usually `linux-x64`), and install it while connected to it.

### Install it
Every editor below has the command **Extensions: Install from VSIX…** (Command Palette, or the `…` menu at the top of the
Extensions view). From a terminal:

| Editor | Install | Uninstall |
|---|---|---|
| VS Code | `code --install-extension goqulient-darwin-arm64.vsix` | `code --uninstall-extension yuji-luigi.goqulient` |
| Cursor | `cursor --install-extension …vsix`, or drag the file into the Extensions view | `cursor --uninstall-extension yuji-luigi.goqulient` |
| Devin Desktop (Windsurf) | the "Install from VSIX…" command; the shell command is now `devin-desktop` (`windsurf` still exists) | Extensions view → GoQulient → Uninstall |
| VSCodium | `codium --install-extension …vsix` | `codium --uninstall-extension yuji-luigi.goqulient` |
| code-server | upload the file first (drag it into the Explorer), then "Install from VSIX…" or `code-server --install-extension …vsix` on that machine | `code-server --uninstall-extension yuji-luigi.goqulient` |
| Ona (Gitpod) | upload the file into the workspace, then "Install from VSIX…" | Extensions view |

The `cursor` and `code` commands come from each editor's **Shell Command: Install '…' command in PATH**. Two details:
- An extension installed from a VSIX doesn't update itself. Once GoQulient is in the store, uninstall the hand-installed
  copy and install it from the store, or you'll stay on the old build.
- Cursor: a command-line install may not appear in a non-default profile; add `--profile <name>` or use the UI.

I tested installing a VSIX with VS Code 1.141 and Cursor 3.27; I didn't test the Devin Desktop and VSCodium command
names. Sources: [VS Code](https://code.visualstudio.com/docs/configure/extensions/extension-marketplace),
[Cursor profiles issue](https://forum.cursor.com/t/install-extension-with-vsix-doesnt-register-in-active-profile/156359),
[code-server FAQ](https://github.com/coder/code-server/blob/main/docs/FAQ.md).

### Check that it works
1. Open the GoQulient view in the activity bar. **GoQulient: Show Engine Version** should show a version.
2. Open a folder whose `.env` has a `DATABASE_URL` (the compose test databases work: `scripts/dev.sh up`), expand the
   connection, open a table and an ER diagram.
3. **GoQulient: Show Log** should show `Starting …/bin/<target>/goku-engine-server` and `Engine … ready`.

## 2. One-time setup

Do these in order. None of them needs Azure, an Azure subscription or an Azure DevOps organization.

### 2.1 Make the repository public
Do this first, for two reasons:
- **README images and links.** The VSIX's README (the store page) points its images and relative links at
  `github.com/yuji-luigi/goqulient/…/vscode/` (vsce's `--baseContentUrl`/`--baseImagesUrl`). While the repository is
  private, the screenshots on the store page are broken images and the links lead to a 404. The stores also need the
  image URLs to be HTTPS (they are).
- **Trusted publishing.** For a private repository, the Marketplace accepts a trusted publishing policy only if the
  workflow runs within 48 hours of creating the policy. A public repository has no such limit.

Before you flip it: `docker-compose.yml` and `docker/` contain test-only passwords, which is fine; change the Atlas test
password (see the [checklist](#6-checklist-before-the-first-public-release)).

### 2.2 Create the Marketplace publisher
1. Go to <https://marketplace.visualstudio.com/manage> and sign in with your Microsoft account.
   **Don't use portal.azure.com, and don't sign up for an Azure free trial or pay-as-you-go plan.** The Azure portal
   is for Azure subscriptions, and publishing to the Marketplace doesn't need one.
2. **Create publisher**: ID `yuji-luigi` (it must equal `publisher` in `vscode/package.json`; the ID can't be changed
   later), name e.g. "yuji-luigi". The other fields are optional.

### 2.3 Add a trusted publishing policy (Marketplace)
Trusted publishing lets the GitHub workflow publish without a stored token: GitHub gives the job a short-lived identity
token (OIDC), and the Marketplace swaps it for a 15-minute publishing credential if it matches your policy. Publishing
runs on GitHub's servers; your computer never holds a Marketplace credential.

The gate is a GitHub **environment**, `marketplace`, that the `publish` job runs in. Create it **before the first
release**: if a workflow names an environment that doesn't exist, GitHub creates it without any protection.
1. GitHub → the repository → **Settings → Environments → New environment**, name `marketplace`, then:
   - **Deployment branches and tags** → **Selected branches and tags** → **Add deployment branch or tag rule** →
     branch `main`. Only runs on `main` can enter the environment.
   - **Required reviewers** → add yourself (and untick "Prevent self-review" if it's ticked, or you can't approve
     your own release). Every publish then waits for you to click **Approve**.
   - Save.
2. On <https://marketplace.visualstudio.com/manage>, open the publisher `yuji-luigi` → **Trusted Publishing** → add a
   policy (or edit the existing one):

   | Field | Value |
   |---|---|
   | Repository owner | `yuji-luigi` |
   | Repository | `goqulient` |
   | Workflow | `release.yml` |
   | Environment | `marketplace` |

3. That's all. The `publish` job in `release.yml` has `environment: marketplace`, `permissions: id-token: write`, and
   runs `npx @vscode/vsce@4 publish --oidc …`.

What this protects: the Marketplace accepts only identity tokens from `release.yml` in this repository **and** from
the `marketplace` environment, which only `main` can enter and which waits for your approval. A `release.yml` changed on
another branch can't publish, pull requests from forks never get an identity token, and there is no token to steal.
What remains is your GitHub account: protect it with two-factor authentication, ideally a passkey or security key.
Optionally, protect `main` too (Settings → Rules → a branch ruleset that requires pull requests), so changes to
`release.yml` go through review.

Why the policy names `release.yml` and not `extension-release.yml`: when a job runs in a called (reusable) workflow,
GitHub's identity token names two files, the caller (`workflow_ref`) and the called one (`job_workflow_ref`), and the
Marketplace docs don't say which one the policy must match. So publishing happens in a job of `release.yml` itself,
where both are `release.yml`.

`--oidc` needs vsce 4.1.0 or later (the workflow uses `@vscode/vsce@4`), and it works only inside GitHub Actions.
Sources: [Publishing extensions, Trusted Publishing](https://code.visualstudio.com/api/working-with-extensions/publishing-extension),
[vsce README](https://github.com/microsoft/vscode-vsce#trusted-publishing),
[GitHub: OIDC with reusable workflows](https://docs.github.com/actions/deployment/security-hardening-your-deployments/using-openid-connect-with-reusable-workflows).

### 2.4 Open VSX account, namespace and token
1. Create an [Eclipse Foundation account](https://accounts.eclipse.org/user/register) and enter your GitHub username in
   it.
2. Sign in at <https://open-vsx.org> with GitHub. On your profile page, **Log in with Eclipse**, then sign the
   **Eclipse Foundation Open VSX Publisher Agreement**.
3. **Settings → Access Tokens → Generate New Token.** Copy it now: it's shown once. Tokens on open-vsx.org expire after
   90 days (you get an email a week before).
4. Create the namespace, which must equal `publisher` in `vscode/package.json` (Docker, nothing installed on the Mac):
   ```bash
   docker run --rm -it node:24-bookworm npx --yes ovsx@0 create-namespace yuji-luigi -p <your-open-vsx-token>
   ```
5. Claim ownership of the namespace: open an issue at
   <https://github.com/EclipseFdn/open-vsx.org/issues/new/choose>. Until it's
   granted, your extension shows a ⚠️ "unverified" warning, and Cursor won't give it a verified badge.

Optional, later: Open VSX also has trusted publishing (`ovsx publish --trusted-publishing`), which would replace
`OVSX_PAT`. It needs namespace ownership and an extension that already has a published version, so the first release
has to use the token anyway. Sources: [Publishing extensions](https://github.com/eclipse/openvsx/wiki/Publishing-Extensions),
[Namespace access](https://github.com/eclipse/openvsx/wiki/Namespace-Access),
[Trusted publishing](https://github.com/eclipse/openvsx/wiki/Trusted-Publishing).

### 2.5 GitHub repository settings
1. **Secret** `OVSX_PAT`: Settings → Secrets and variables → Actions → New repository secret, value
   `<your-open-vsx-token>`. Renew it before it expires (90 days). Without it, the Open VSX step only warns.
2. **Let release-please open its PR**: Settings → Actions → General → Workflow permissions → tick **Allow GitHub
   Actions to create and approve pull requests** → Save. Without it, release-please fails with "GitHub Actions is not
   permitted to create or approve pull requests".
3. `VSCE_PAT` is no longer used. If you created it, delete it.

### Legacy: Azure DevOps personal access tokens
Until now, publishing to the Marketplace from outside GitHub Actions meant an Azure DevOps personal access token
(scope **Marketplace → Manage**, organization **All accessible organizations**). Don't build on it:
- Those tokens are "global" tokens, which Microsoft is retiring. The [VS Code docs](https://code.visualstudio.com/api/working-with-extensions/publishing-extension)
  say 2026-12-01. On 2026-10-23 the [Azure DevOps blog](https://devblogs.microsoft.com/devops/retirement-of-global-personal-access-tokens-in-azure-devops/)
  postponed the date to after the December holidays, but the new date in the post is garbled ("January 27, 2026"),
  so check the post if you rely on it.
- Creating the Azure DevOps organization the token comes from now [needs an active Azure subscription](https://learn.microsoft.com/en-us/azure/devops/release-notes/2025/general/sprint-265-update)
  (since 2025-11-25).

Use trusted publishing instead (2.3).

## 3. Publishing

### 3.1 The normal flow (release-please)
1. Merge pull requests with [Conventional Commits](https://www.conventionalcommits.org/) titles (`feat:`, `fix:` …).
2. release-please keeps a **release PR** open ("chore(main): release X.Y.Z") with the changelog and version bumps.
3. Merge it. `release.yml` tags `vX.Y.Z`, creates the GitHub release, and builds and packages the six targets.
4. The `publish` job then **waits for your approval**: open the run (Actions → Release, or the email/notification
   "Review pending deployments") → **Review deployments** → tick `marketplace` → **Approve and deploy**. It
   publishes to both stores, as pre-releases until 1.0.
5. Check the run's summary, then the [Marketplace page](https://marketplace.visualstudio.com/items?itemName=yuji-luigi.goqulient)
   and [Open VSX page](https://open-vsx.org/extension/yuji-luigi/goqulient). Cursor's mirror can take hours, sometimes
   about a day, to show a new version.

Publishing is **all or nothing**: if any of the six targets is missing, the `publish` job stops before uploading
anything. Fix the cause, then publish that tag by hand (3.2).

### 3.2 Publishing by hand from GitHub
- **Build only:** Actions → **Extension release** → Run workflow. Input `prerelease` (default on) packages as a
  pre-release. It never publishes.
- **(Re)publish a release:** Actions → **Release** → Run workflow, from branch **`main`** (the environment refuses
  other branches), with:
  - `tag`: an existing tag, e.g. `v0.2.0`;
  - `prerelease`: on for a pre-release, off for a stable release.

  It builds that tag and runs the same `publish` job, which waits for your approval. Both stores use
  `--skip-duplicate`, so after a partial failure (e.g. the Marketplace worked and Open VSX didn't) a re-run publishes
  only what's missing.

### 3.3 Pre-release or stable
- The stores don't accept version suffixes like `1.0.0-beta`; a pre-release is a normal version with a pre-release
  flag. A pre-release and a stable release can't share a version number.
- Microsoft recommends **odd minor versions for pre-releases and even ones for stable releases** (0.3.x pre-release,
  0.4.x stable). Users on the pre-release channel always get the highest version, stable or not.
- VS Code users see a pre-release only if they choose it: Install ▾ → **Install Pre-Release Version**, or
  **Switch to Pre-Release Version** on an installed extension (`code --install-extension yuji-luigi.goqulient --pre-release`).
- Until 1.0, `release.yml` publishes pre-releases, on the odd minor **0.1.x**: release-please bumps the patch for
  `feat:` and `fix:` (`bump-patch-for-minor-pre-major`). A breaking change (`feat!:`) bumps the minor, to an even
  0.2.0: before merging that release PR, set `"release-as"` to the next odd minor (0.3.0), and remove it after the
  release.
- To publish a stable release, use an even minor (`release-as`) and run **Release** by hand with `prerelease` off, or
  change `prerelease: true` in `release.yml`.

### 3.4 Publishing a VSIX from your computer (fallback)
- **Open VSX** works from any computer with the token:
  ```bash
  docker run --rm -v "$PWD/dist/vsix:/vsix:ro" node:24-bookworm sh -c \
    'for f in /vsix/*.vsix; do npx --yes ovsx@0 publish "$f" --pre-release -p <your-open-vsx-token>; done'
  ```
  `--pre-release` only works with VSIXs packaged as pre-releases (the workflow's default).
- **Marketplace:** `vsce publish --oidc` works only in GitHub Actions. From your computer, `vsce publish --packagePath
  <files…>` needs a legacy token (`-p`, see above) or `--azure-credential` (Microsoft Entra ID, which needs an Azure
  tenant). So in practice, publish to the Marketplace through the workflow (3.2). As a last resort, the Marketplace
  web page can upload a single `.vsix` (**+ New extension** → Visual Studio Code); I couldn't confirm that it accepts
  the other platform builds of an existing extension.

### 3.5 How the six builds appear in the stores
- **Marketplace:** one extension page for all six. VS Code 1.61 and later downloads the build for its own platform.
  There is no universal build (one packaged without `--target`), so a platform without a build, e.g. Linux on 32-bit
  ARM, can't install GoQulient.
- **Open VSX:** one page as well, with the same per-platform builds; Cursor, VSCodium and code-server download the one
  for their platform.

### 3.6 Unpublish, remove a version, deprecate
| Goal | Marketplace | Open VSX |
|---|---|---|
| Hide the whole extension, reversibly | manage page → GoQulient → More actions → **Unpublish** (keeps statistics) | — |
| Delete the whole extension | More actions → **Remove**, or `vsce unpublish yuji-luigi.goqulient`: irreversible, and the name is reserved forever, even for you | web page, or `ovsx unpublish yuji-luigi.goqulient` (namespace owner) |
| Delete one bad version | Reports → Manage → **Delete this version** (not the latest; the number can't be reused) | web page delete, or `ovsx unpublish yuji-luigi.goqulient --versions 0.3.1` (number stays reserved) |
| Point users to a replacement | comment in the Marketplace's "Deprecated extensions" discussion; VS Code then shows it struck through | — |

Usually the right fix for a bad release is a **new patch version**, not a deletion. Sources:
[Marketplace](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#unpublishing-extensions),
[Open VSX](https://github.com/eclipse/openvsx/wiki/Deleting-Extensions).

## 4. Reading GoQulient: Show Log
**GoQulient: Show Log** opens the "GoQulient" output channel:
- `Starting <path>`: the engine binary the extension runs (`…/bin/<target>/goku-engine-server`, or your
  `goqulient.enginePath`).
- `[engine] …`: the engine's own messages (its stderr).
- `Engine X.Y.Z ready (protocol N)`: the engine started and answered.
- `The GoQulient engine stopped unexpectedly (…)`: it crashed or was killed; the extension restarts it a few times,
  then gives up ("crashed repeatedly"). **GoQulient: Restart Engine** tries again.
- `Couldn't start the engine: …`: it never started; the message says why (see below).

Error messages and the log never contain passwords. When a user reports a problem, ask for this log and the output of
**GoQulient: Show Engine Version**.

## 5. Troubleshooting

### Setting up and publishing
| Symptom | Cause | Fix |
|---|---|---|
| Microsoft sign-in error **AADSTS50020**: "The selected user account does not exist in tenant 'Microsoft Services' and cannot access the application 'c44b4083-…'" | You went to the Azure portal (portal.azure.com, that app id) with an account that has no Azure directory or subscription. | You don't need the Azure portal. Create the publisher at <https://marketplace.visualstudio.com/manage> (2.2) and use trusted publishing (2.3). |
| Azure DevOps asks for an **Azure subscription** when you create an organization | Since 2025-11-25, new Azure DevOps organizations need one. You only need an organization for a legacy token. | Don't create it. Trusted publishing needs no organization, token or subscription. |
| You're pushed to an **Azure free trial / pay-as-you-go** sign-up | Wrong path: that's Azure, not the Marketplace. | Close it. Use the Marketplace manage page and trusted publishing. |
| Publish step: "GitHub Actions did not provide an OIDC token request URL and token. Add `permissions: id-token: write`…" | The job lacks `id-token: write`, or publishing moved to another job. | Keep publishing in the `publish` job of `release.yml`, which has the permission. |
| Publish step: "Marketplace OIDC token exchange" fails (401/403) | No policy, or it doesn't match: owner, repository, workflow file (`release.yml`), environment (`marketplace`), or a private repo past the 48-hour window. | Fix the policy (2.3). For a private repo, re-create it and run the workflow within 48 hours, or make the repo public first. |
| The `publish` job sits at "Waiting" | It's waiting for your approval (the `marketplace` environment's required reviewer). | Open the run → Review deployments → Approve and deploy. |
| `publish` fails at once: branch "…" is not allowed to deploy to marketplace due to environment protection rules | The run isn't on `main` (e.g. **Release** was run by hand from another branch). | Run it from `main`, with the tag as input. |
| "No supported OIDC provider was detected" | `--oidc` was run outside GitHub Actions. | Publish to the Marketplace through the workflow (3.2). |
| Open VSX: 401/403, or an "invalid token" error | `OVSX_PAT` expired (90 days), was revoked, or was pasted with extra spaces. | Generate a new token (2.4) and update the secret. Check it with `npx ovsx verify-pat yuji-luigi -p <token>`. |
| Legacy token: 401/403 from `vsce publish -p` | Expired, or wrong scope: it needs Marketplace → **Manage** and **All accessible organizations**. | Move to trusted publishing. |
| An error naming the publisher or namespace (unknown publisher, not a member of the namespace) | `publisher` in `vscode/package.json` isn't exactly the Marketplace publisher ID / Open VSX namespace (`yuji-luigi`), or your account isn't a member of it. | Make all three identical (the field is case-sensitive on Open VSX); create the namespace with your token's account. |
| "already exists" when publishing for the first time | The Marketplace needs a unique extension `name` **and** a unique display name; someone else has one of them. | Change `displayName` (and, if needed, `name`) before the first publish: the extension ID (`yuji-luigi.goqulient`) is permanent. |
| Version already exists | That version (or a deleted one) was already published. | Release a new version; deleted numbers can't be reused. A re-run of the same tag skips duplicates. |
| vsce warns that LICENSE or the repository field is missing | The VSIX was packaged without `vscode:prepublish` (it copies the root `LICENSE`), or `repository` was removed from `package.json`. | Package with `scripts/package-vsix.sh` or `scripts/dev.sh vscode package`; keep `repository` and `license` in `package.json`. |
| Store page shows broken images and links lead to a 404 | The repository is private: images and links point at GitHub. | Make the repository public (2.1). No re-publish needed: the URLs start working. |
| Store page has no icon (the default placeholder) | `vscode/package.json` has no `icon`. | Add a PNG of at least 128×128 px (not SVG) and `"icon": "media/icon.png"`; see the checklist. |
| Local packaging: "The following files have the same case insensitive path, which isn't supported by the VSIX format: extension/readme.md, extension/readme.md" | vsce ran in a folder Docker shares from the Mac. That disk is case-insensitive: once something has looked up `readme.md`, the Linux side lists `README.md` under both spellings, and vsce sees two READMEs. It only happens from the second package in the same folder, so it looks like a problem with the second target. It never happens on GitHub's Linux runners. | Use `scripts/dev.sh vscode package`, which packages a copy on the container's own disk. |
| Packaging: `ENOENT … open '…/dist/goqulient-<target>.vsix'` | vsce doesn't create the `-o` folder. | `scripts/package-vsix.sh` creates it. |
| A macOS engine job is **cancelled** after waiting, "The job was not acquired by Runner of type hosted even after multiple attempts" | GitHub's hosted macOS runners are short of capacity. | Both macOS targets now build on one runner, so this is rarer. The other targets are still packaged; re-run the failed job (Re-run failed jobs), then publish the tag (3.2). |
| `publish` fails: "Not publishing: no VSIX for darwin-x64" | An engine job failed; publishing is all or nothing. | Re-run the failed jobs, or run **Release** by hand with the tag. |
| release-please: "GitHub Actions is not permitted to create or approve pull requests" | The repository setting is off. | 2.5, step 2. |

### Installing and running
| Symptom | Cause | Fix |
|---|---|---|
| GoQulient isn't found in Cursor's Extensions search | Cursor's mirror syncs from Open VSX periodically (hours, sometimes about a day); or the version is pre-release only and Cursor's UI may not offer pre-releases; or the extension's `engines.vscode` is newer than Cursor's VS Code base (Cursor 3.27 is based on 1.128). | Wait and check <https://open-vsx.org/extension/yuji-luigi/goqulient> first. Meanwhile, install the VSIX by hand. Keep `engines.vscode` at or below Cursor's base. |
| Not found in VS Code / VSCodium search | Not published to that store yet, or only as a pre-release and the search hides it. | Check the store page. |
| Only an old version appears, or "Update" is missing | The copy was installed from a VSIX; those don't update. | Uninstall it and install from the store. |
| "The GoQulient engine for darwin-arm64 is missing from this installation. Reinstall the extension." (any target) | The wrong platform's VSIX was installed (e.g. the Intel build on Apple silicon: the extension looks for `bin/<its own platform>/`), or the install is incomplete. | Uninstall, then install the right file (1. Pick the right file) or from the store, which picks the right build. |
| "Unable to install extension … as it is not compatible with VS Code …" | The editor is older than `engines.vscode` (`^1.95.0`). | Update the editor, or for a very old fork lower `engines.vscode` and `@types/vscode` together (and test). |
| The pre-release version doesn't show up | Pre-releases are opt-in. | VS Code: Install ▾ → Install Pre-Release Version, or `--pre-release` on the command line. |
| macOS: "goku-engine-server can't be opened because Apple cannot check it…", or the engine is killed right away | The binary isn't signed or notarized. VS Code and Cursor don't add the quarantine flag to files they unpack (tested with VS Code 1.141 and Cursor 3.27), so this only happens when the files were copied or unpacked by other tools. | `xattr -dr com.apple.quarantine ~/.vscode/extensions/yuji-luigi.goqulient-*/bin` (Cursor: `~/.cursor/extensions/…`), then GoQulient: Restart Engine. Long term: sign and notarize the macOS binaries. |
| Windows: the engine disappears, or the log shows `spawn … ENOENT` / `EPERM` / "Access is denied" | Microsoft Defender quarantined the unsigned `goku-engine-server.exe` (Windows Security → Protection history), or Smart App Control (Windows 11) blocked an unsigned program. SmartScreen itself doesn't apply: the editor starts the engine directly, not through Explorer. | Restore and allow the file in Protection history, then reinstall the extension. Report false positives to Microsoft. Long term: sign the Windows binaries. |
| Linux: `EACCES` / "permission denied" | The exec bit was lost (the extension restores it, so this is rare), or the extensions folder is on a `noexec` mount. | `chmod +x ~/.vscode*/extensions/yuji-luigi.goqulient-*/bin/*/goku-engine-server`; for `noexec`, move the extensions folder (`--extensions-dir`) or remount. The Linux engine is a static musl binary: it doesn't depend on the distribution's libraries, Alpine included. |
| "The GoQulient engine crashed repeatedly" | The engine keeps exiting. | GoQulient: Show Log (section 4), then GoQulient: Restart Engine. Report the log in an issue. |

## 6. Checklist before the first public release
- [ ] **Icon (TODO, not chosen yet):** add a square PNG of at least 128×128 px (256×256 recommended, no SVG) as
      `vscode/media/icon.png` and `"icon": "media/icon.png"` in `vscode/package.json`. Without it, both stores show a
      placeholder. This is your choice of logo, so it's left open on purpose.
- [ ] **Change the Atlas test password**: give the MongoDB Atlas user of the optional Atlas test
      ([TESTING.md](TESTING.md)) a new password before the repository goes public, and keep it only in your untracked
      `.env.atlas`. (This repository's git history contains no Atlas password; the old one may have been used or
      shared elsewhere.)
- [x] Make the repository public (2.1), then check that the README images load on GitHub.
- [x] Create the Marketplace publisher `yuji-luigi` (2.2), the GitHub environment `marketplace` (only `main`, you as
      required reviewer) and the trusted publishing policy for `release.yml` with environment `marketplace` (2.3).
      The GitHub side is also kept as Terraform, in a separate project (`vscode_extension_marketplace_github`).
- [ ] Two-factor authentication on your GitHub account.
- [ ] Open VSX: agreement, token, namespace `yuji-luigi`, ownership claim (2.4). Postponed: until then, Cursor,
      Devin Desktop, VSCodium and code-server users install the VSIX by hand.
- [x] GitHub: the release-please setting (2.5). `OVSX_PAT` waits for Open VSX.
- [x] First version: 0.1.0, a pre-release (odd minor), released on 2026-10-29 with `"release-as": "0.1.0"`, which was
      removed afterwards.
- [ ] Try the six VSIXs from an **Extension release** run in VS Code and Cursor (section 1), at least on darwin-arm64
      and one Windows or Linux machine.
- [ ] Re-read `vscode/README.md` (it becomes the store page) and `CHANGELOG.md`.
- [ ] After the first release: install from each store in VS Code and Cursor, and check both store pages.
