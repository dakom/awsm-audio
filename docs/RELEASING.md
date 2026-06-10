# Releasing awsm-audio

This repo ships **three independent release artifacts**, each on its own track —
releasing one doesn't touch the others:

| # | Artifact | Trigger | Destination |
|---|----------|---------|-------------|
| 1 | **Frontend editor** | `task editor:deploy` | Cloudflare Pages |
| 2 | **Library crates** (`schema`, `player`, `worklet`) | `task crates:publish` | crates.io |
| 3 | **MCP server** (`awsm-audio-mcp`) | push a `v<version>` git tag | GitHub Releases |

Versions across the workspace move in lockstep (`version` under
`[workspace.package]` in the root `Cargo.toml`), but the three tracks are
published independently.

---

## 1. Frontend editor → Cloudflare Pages

```sh
task editor:deploy     # production trunk build + wrangler deploy (project: awsm-audio)
```

Needs `CLOUDFLARE_DEPLOY_WORKERS_TOKEN` in the repo-root `.env`; the project name
and branch come from `taskfiles/config.yml`. The task ensures the Pages project
exists, then deploys the built `.build-artifacts/editor` tree.

> Independently, every push to `main` also publishes the editor to **GitHub Pages**
> (`https://dakom.github.io/awsm-audio/editor/`) via `.github/workflows/pages.yml`
> — that one is automatic, no command needed.

## 2. Library crates → crates.io

```sh
task crates:publish-dry-run     # package + verify, upload nothing
task crates:publish             # publish for real
```

Publishes the three creator-facing crates, in dependency order:
`awsm-audio-schema` → `awsm-audio-player`, plus the standalone
`awsm-audio-worklet`. The editor, the MCP server, `editor-protocol`, and the
example worklets are all `publish = false` and never go out — the publish task
lists the crates explicitly (not `--workspace`) so that stays true as new internal
members are added.

## 3. MCP server → GitHub Releases

The native MCP server ships as prebuilt binaries on **GitHub Releases**, driven by
[cargo-dist](https://opensource.axo.dev/cargo-dist/). A release is triggered by
pushing a **version git tag**; CI builds every platform and publishes the binaries
plus the `curl`/PowerShell installers.

### Cut a release

1. **Bump the version.** The workspace versions in lockstep — edit
   `version` under `[workspace.package]` in the root `Cargo.toml` (e.g.
   `0.1.0` → `0.1.1`). Commit it (`cargo build` once so `Cargo.lock` updates too).

2. **Sanity-check the dist plan** (optional but cheap — no network):

   ```sh
   dist plan          # shows the artifacts/installers that will be produced
   ```

3. **Tag and push.** The tag must be `v<version>` and match the Cargo version:

   ```sh
   git tag -a v0.1.1 -m "awsm-audio-mcp v0.1.1"
   git push origin v0.1.1
   ```

   That's it — pushing the tag starts the **Release** workflow
   (`.github/workflows/release.yml`). Watch it with `gh run watch` or on the
   Actions tab.

4. **Verify** (a minute after it goes green):

   ```sh
   gh release view v0.1.1                       # binaries + installers attached
   ```

The tag is what matters, not the branch — release from `main` once a change has
landed there. (You *can* tag any commit; CI builds whatever the tag points at.)

### What the workflow produces

A published GitHub Release at `…/releases/tag/v<version>` with:

- per-platform archives: macOS arm64 + x86_64, Linux x86_64, Windows x86_64-msvc
  (`.tar.xz` / `.zip`) plus `.sha256` checksums,
- `awsm-audio-mcp-installer.sh` (the `curl … | sh` installer) and
  `awsm-audio-mcp-installer.ps1` (PowerShell).

The README's install commands all point at `releases/latest/download/…`, so they
keep working across versions with no edits.

### One-time setup (already done — for reference / re-creation)

- **`[workspace.metadata.dist]`** in the root `Cargo.toml` holds the dist config
  (targets, installers, `precise-builds`). `precise-builds = true` is
  important: it builds only the `awsm-audio-mcp` package, so dist never tries to
  host-compile the wasm-only editor crate. The editor opts out via
  `[package.metadata.dist] dist = false`; the MCP crate opts in with `dist = true`
  (it's `publish = false`, which dist otherwise treats as "don't ship").

### Changing the dist config

Edit `[workspace.metadata.dist]` (or add/remove installers/targets), then
regenerate the CI workflow so it stays in sync:

```sh
dist init --yes     # rewrites [workspace.metadata.dist] canonically + regenerates CI
dist generate       # just regenerate .github/workflows/release.yml from the config
```

Commit the regenerated `release.yml` alongside the config change. Bumping the
pinned `cargo-dist-version` is how you upgrade the toolchain CI uses.
