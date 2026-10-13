# Releases, CI and updates

This page describes the release tooling (PLAN 4 and 6.8). Nothing here creates a tag or a release by itself: a maintainer pushes the tag.

## Workflows

| File | Runs on | What |
|---|---|---|
| `ci.yml` | pull requests, pushes to main | the lean `quick` job, Linux only: fmt, clippy `-D warnings`, `cargo test --workspace`, timeline vectors, spec fast checks |
| `full.yml` | nightly (02:17 UTC) and on demand | the spec and fuzz checks; `rust-matrix` (`cargo test --workspace` and `cargo deny check bans licenses sources` on ubuntu, windows and macOS); `sandbox-linux` (installs bubblewrap, runs han-host's `sandbox_probe`) |
| `full.yml`, job `advisories` | daily (05:43 UTC) and on demand | `cargo deny check advisories` and `cargo audit`, because a new advisory can appear without any change of ours. The reviewed ignore is in `deny.toml` and repeated in the audit command |
| `release.yml` | tags `v0.*` only, never pull requests | cargo-dist: plan, build per target, global installers, GitHub Release |
| `toolchain-bump.yml` | monthly (the 1st) and on demand | opens a PR moving `rust-toolchain.toml` to the latest stable |
| `dependabot.yml` | weekly | PRs that update the pinned GitHub Actions |

Every action is pinned to a full commit SHA with its version in a trailing comment (`uses: owner/action@<sha> # v1.2.3`). Dependabot moves both together. Add new actions the same way; resolve the SHA of the release tag (for an annotated tag, the commit it points to).

## Cutting a release (maintainer)

1. Set the workspace version in `Cargo.toml` (`[workspace.package] version`) and the matching `version = "..."` in the `workspace.dependencies` path entries; merge.
2. `git tag v0.X.Y && git push origin v0.X.Y`. The `plan` job fails if the tag does not match the crate version.
3. `release.yml` builds the archives (`.tar.xz` on Unix, `.zip` on Windows, with `.sha256` files), the shell installer, the Homebrew formula and the MSI, and creates the GitHub Release.

Targets (`dist-workspace.toml`): x86_64 and aarch64 Linux (the aarch64 build runs natively on `ubuntu-24.04-arm`, no cross-compiling), x86_64 and aarch64 macOS, x86_64 Windows. Windows aarch64 is left out until there is a reason to ship it. Try a plan locally with `cargo install cargo-dist --locked` and `dist plan`.

`release.yml` is written by hand after dist's template and `dist-workspace.toml` sets `allow-dirty = ["ci"]` so dist accepts that. It has not been run end to end yet: run it first on a tag in a fork, or regenerate it with `dist generate` and re-apply the SHA pins and the TODO jobs.

## What needs maintainer secrets (all left as marked TODOs)

| Secret or setting | For | Where |
|---|---|---|
| `HOMEBREW_TAP_TOKEN` (secret), plus the repository `lmvdz/homebrew-tap` | publishing the Homebrew formula | job `publish-homebrew` in `release.yml` (disabled with `if: false`) |
| `HAN_UPDATE_SIGNING_KEY` (secret) | signing the update manifest | job `sign-manifest` in `release.yml` (disabled with `if: false`) |
| `HAN_UPDATE_PUBKEY` (repository variable, not secret) | the public key embedded in release builds | `build-local` in `release.yml` |
| Authenticode and Apple certificates | signing the MSI and macOS binaries | not wired up |
| `BUMP_TOKEN` (secret, optional) and the "Allow GitHub Actions to create pull requests" setting | making toolchain-bump PRs start CI | `toolchain-bump.yml` |

## Self-update

`han update --check --manifest latest.json` verifies `latest.json.sig` (a detached Ed25519 signature) over the manifest and says whether a newer version exists. It reads local files only, downloads nothing and installs nothing: the binary never updates itself without an explicit action by the user, and for now an upgrade means running the installer again (or `brew upgrade`). Applying an update is not implemented.

- The manifest is JSON: `{"version": "0.2.0", "notes_url": "..."}` (`notes_url` optional). Versions are plain `MAJOR.MINOR.PATCH`.
- The signature is over `han-release-manifest/1\n` followed by the exact manifest bytes, as unpadded base64url. The prefix keeps a manifest signature from being valid for anything else the key signs. Verification happens before the manifest is parsed.
- The public key is the constant `UPDATE_PUBLIC_KEY` in `apps/han/src/update.rs`: a placeholder in the source, replaced at build time by the environment variable `HAN_UPDATE_PUBKEY`. A build with the placeholder refuses to check.

Generating the key pair (needs `pip install cryptography`):

```sh
python ref/release_keys.py keygen
```

It prints the private seed (64 hex digits) and the public key. Store the seed as the secret `HAN_UPDATE_SIGNING_KEY` and nowhere else; set the public key as the repository variable `HAN_UPDATE_PUBKEY`. To sign by hand:

```sh
HAN_UPDATE_SIGNING_KEY=<seed> python ref/release_keys.py sign --version 0.2.0 --out latest.json
HAN_UPDATE_PUBKEY=<public key> cargo run -p han -- update --check --manifest latest.json
```

Rotating the key means shipping a release built with the new public key while the old key still signs the manifest that announces it.
