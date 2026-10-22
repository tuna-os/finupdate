# Releasing Finupdate

A Finupdate release has one version identity: `v<version>`. The Git tag,
GitHub Release, changelog heading, and versioned OCI artifact tags must all
come from the same commit.

## Release contract

- `Cargo.toml` contains the release version, currently `0.1.0`.
- `CHANGELOG.md` contains a `## [<version>]` entry for that version.
- The tag is `v<version>` and points at a commit with a successful `CI` run.
- The publish workflow creates or publishes the GitHub Release before the
  reusable Flatpak workflow uploads bundles.
- The reusable workflow publishes these OCI tags for a version tag such as
  `v0.1.0`:
  - `v0.1.0-x86_64`
  - `v0.1.0-aarch64`
  - `latest`, `latest-x86_64`, and `latest-aarch64` for the current remote
- The central Flatpak index continues to follow `latest`. Consumers that need
  an immutable OCI reference should use the architecture-specific version tag
  matching their target machine.

The architecture suffix is part of the artifact identity; the `v<version>`
prefix is never replaced with `latest` or a bare version number.

## Cut a release

Run the normal checks on the exact commit that will be tagged:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- \
  -D clippy::correctness \
  -D clippy::suspicious \
  -W clippy::style \
  -W clippy::complexity \
  -W clippy::perf \
  -A deprecated \
  -A unused
GDK_BACKEND=broadway BROADWAY_DISPLAY=:0 cargo test --workspace --all-targets
```

Confirm the working tree is clean and that the version and changelog agree:

```sh
VERSION=$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)
test "$VERSION" = "0.1.0"
grep -Fq "## [$VERSION]" CHANGELOG.md
```

Create and push an annotated tag for the tested commit:

```sh
git tag -a "v$VERSION" -m "Release v$VERSION" HEAD
git push upstream "v$VERSION"
```

`Publish Flatpak` then performs the release gate. It verifies the tag and
changelog version, requires a successful `CI` run for the tagged commit, and
creates or publishes the GitHub Release before the reusable workflow builds
the x86_64 and aarch64 OCI/Flatpak matrix and attaches both Flatpak bundles.

## Verify a published release

Run all of these checks after the workflow completes:

```sh
git ls-remote --tags upstream 'refs/tags/v0.1.0'
GH_TOKEN="$GH_TOKEN" gh release view v0.1.0 --repo tuna-os/finupdate
skopeo inspect docker://ghcr.io/tuna-os/finupdate:v0.1.0-x86_64
skopeo inspect docker://ghcr.io/tuna-os/finupdate:v0.1.0-aarch64
GH_TOKEN="$GH_TOKEN" gh release view v0.1.0 \
  --repo tuna-os/finupdate --json assets \
  --jq '.assets[].name'
```

The release must be visible and not a draft. The tag check must return the
commit that passed CI. The two `skopeo` checks must resolve the matching
architecture-specific release tags, and the GitHub Release must list both
architecture-specific `.flatpak` bundles.

Downstream installers, image builders, and registry consumers can pin the
release by selecting `v0.1.0-x86_64` or `v0.1.0-aarch64`. Flatpak users of the
TunaOS remote continue to receive the index's `latest` entry.

For an incident after publication, use
[the rollback runbook](../runbooks/rollback-a-bad-finupdate-release.md).
