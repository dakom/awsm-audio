# Releasing `awsm-audio-mcp`

The native MCP server ships as prebuilt binaries on **GitHub Releases**, driven by
[cargo-dist](https://opensource.axo.dev/cargo-dist/). A release is triggered by
pushing a **version git tag**; CI builds every platform and publishes the binaries,
the `curl`/PowerShell installers, and the Homebrew formula.

Only `awsm-audio-mcp` is distributed this way. The library crates (`schema`,
`player`, `worklet`) go to crates.io via `task publish`; the editor deploys to
Cloudflare Pages. Those are separate from this flow.

## Cut a release

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
   gh api repos/dakom/homebrew-tap/contents/Formula/awsm-audio-mcp.rb -q .name
   ```

The tag is what matters, not the branch — release from `main` once a change has
landed there. (You *can* tag any commit; CI builds whatever the tag points at.)

## What the workflow produces

A published GitHub Release at `…/releases/tag/v<version>` with:

- per-platform archives: macOS arm64 + x86_64, Linux x86_64, Windows x86_64-msvc
  (`.tar.xz` / `.zip`) plus `.sha256` checksums,
- `awsm-audio-mcp-installer.sh` (the `curl … | sh` installer) and
  `awsm-audio-mcp-installer.ps1` (PowerShell),
- `awsm-audio-mcp.rb` (the Homebrew formula),

and a commit to **`dakom/homebrew-tap`** adding/updating
`Formula/awsm-audio-mcp.rb`, so `brew install dakom/tap/awsm-audio-mcp` resolves.

The README's install commands all point at `releases/latest/download/…`, so they
keep working across versions with no edits.

## One-time setup (already done — for reference / re-creation)

- **`[workspace.metadata.dist]`** in the root `Cargo.toml` holds the dist config
  (targets, installers, `tap`, `precise-builds`). `precise-builds = true` is
  important: it builds only the `awsm-audio-mcp` package, so dist never tries to
  host-compile the wasm-only editor crate. The editor opts out via
  `[package.metadata.dist] dist = false`; the MCP crate opts in with `dist = true`
  (it's `publish = false`, which dist otherwise treats as "don't ship").
- **The tap repo** `dakom/homebrew-tap` is a *single, shared* tap for all of
  dakom's tools — each project drops its own `Formula/<name>.rb` into it.
- **`HOMEBREW_TAP_TOKEN`** secret on the `awsm-audio` repo: a fine-grained PAT with
  *Contents: read/write* on `dakom/homebrew-tap` only. The built-in `GITHUB_TOKEN`
  can't push to another repo, hence this. Set via repo Settings → Secrets, or
  `gh secret set HOMEBREW_TAP_TOKEN -R dakom/awsm-audio`.

### Gotcha: a brand-new tap repo must not be empty

A freshly-created GitHub repo has **no `main` branch** until its first commit, and
the Homebrew publish job checks out `main` to push the formula — so the *first*
release fails with `couldn't find remote ref refs/heads/main`. Fix once by giving
the tap an initial commit (a README is enough); every release after that just
works. If it happens again on a new tap:

```sh
gh repo clone dakom/homebrew-tap /tmp/tap && cd /tmp/tap
git switch -c main && echo "# homebrew-tap" > README.md
git add -A && git commit -m "Initialize tap" && git push -u origin main
gh run rerun <failed-run-id> --failed   # re-run just the homebrew job
```

## Changing the dist config

Edit `[workspace.metadata.dist]` (or add/remove installers/targets), then
regenerate the CI workflow so it stays in sync:

```sh
dist init --yes     # rewrites [workspace.metadata.dist] canonically + regenerates CI
dist generate       # just regenerate .github/workflows/release.yml from the config
```

Commit the regenerated `release.yml` alongside the config change. Bumping the
pinned `cargo-dist-version` is how you upgrade the toolchain CI uses.
