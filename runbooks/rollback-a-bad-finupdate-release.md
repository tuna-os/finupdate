# Runbook: roll back a bad finupdate release

**Use when:** a build published to the TunaOS Flatpak remote is broken for
users — it crashes on launch, fails to elevate, reports wrong update state, or
otherwise has to stop reaching machines now.

**Do not use for:** a failed publish run. If `Publish Flatpak` itself failed,
nothing new reached the remote and users are still on the previous build —
fix forward on `main` instead.

---

## What "released" means in this repo

- `.github/workflows/publish-flatpak.yml` fires on **every push to `main`**,
  on `v*` tags, and on manual dispatch. There is no release branch, no
  approval step, and the workflow does not wait for `CI` — the two workflows
  start in parallel on the same push, so a green publish is not evidence that
  the test suite passed for that commit.
- Published images land in `ghcr.io/tuna-os/finupdate` under the **mutable**
  tags `latest`, `latest-x86_64` and `latest-aarch64`. Previous builds stay in
  the registry as untagged digests; they are still pullable by digest, but
  nothing in this repository records which digest came from which commit.
- The user-facing remote (`https://tunaos.org/flatpak/tuna-os.flatpakrepo`) is
  an OCI index over those tags, maintained in `tuna-os/flatpak-index`.
- A publish run takes roughly **6–25 minutes** end to end. Treat that as the
  floor for rollback time on the primary path below.

---

## 1. Confirm what is live

```sh
# Most recent publish runs, newest first — note the head SHA and finish time.
gh run list --repo tuna-os/finupdate --workflow "Publish Flatpak" --limit 10 \
  --json headSha,conclusion,createdAt,updatedAt \
  --jq '.[] | "\(.updatedAt)  \(.headSha[0:8])  \(.conclusion)"'

# What the tags currently point at.
gh api "orgs/tuna-os/packages/container/finupdate/versions?per_page=20" \
  --jq '.[] | "\(.updated_at)  \(.name)  tags=\(.metadata.container.tags | join(","))"'
```

The image version whose `updated_at` matches the finish time of the newest
successful publish run is the build users are getting. Write down both the
digest and the commit SHA — the timestamp match is currently the only link
between them.

## 2. Identify the last-good build

Walk back through the publish runs to the newest one you are willing to ship,
and pair it with the image digest published at the same time. Record:

- `BAD_SHA` — the commit that produced the live build
- `GOOD_SHA` — the last commit known good
- `GOOD_DIGEST` — the image digest published from `GOOD_SHA`

## 3. Roll back

### Primary path — revert on `main` (always works)

```sh
git fetch origin main
git checkout -B rollback/revert-BAD_SHA origin/main
git revert --no-edit <BAD_SHA>          # add --mainline 1 if it was a merge
git push origin rollback/revert-BAD_SHA
```

Open a PR and get it into `main`. The push to `main` re-triggers
`Publish Flatpak`, which republishes over the mutable tags:

```sh
gh run watch --repo tuna-os/finupdate "$(gh run list --repo tuna-os/finupdate \
  --workflow 'Publish Flatpak' --limit 1 --json databaseId --jq '.[0].databaseId')"
```

A revert is preferred over a force-push: it keeps the bad commit in history so
the follow-up fix can be reviewed against it.

### Alternative — republish the last-good commit without touching `main`

`workflow_dispatch` accepts a branch or tag, not a bare SHA, so tag the
last-good commit first. A `rollback-*` tag does not match the workflow's `v*`
push trigger, so pushing the tag does not itself publish — the dispatch does:

```sh
git tag rollback-$(date +%Y%m%d-%H%M) <GOOD_SHA>
git push origin rollback-<the tag you just made>
gh workflow run "Publish Flatpak" --repo tuna-os/finupdate \
  --ref rollback-<the tag you just made>
```

Use this when the revert is not clean, or when `main` has already moved on and
you want the known-good tree back on the remote first and the code fix after.

Note: `main` is now ahead of what is published. The **next** push to `main`
republishes it, bad commit included — land the revert or the fix before
anything else merges.

## 4. Verify the rollback

```sh
# Tags moved to the expected digest.
gh api "orgs/tuna-os/packages/container/finupdate/versions?per_page=10" \
  --jq '.[] | select(.metadata.container.tags | length > 0)
        | "\(.updated_at)  \(.name)  tags=\(.metadata.container.tags | join(","))"'
```

Then on a test machine that has the bad build installed:

```sh
flatpak update org.tunaos.finupdate
flatpak info org.tunaos.finupdate      # check Commit and Date changed
flatpak run org.tunaos.finupdate       # reproduce the original symptom — it should be gone
```

If the registry tags moved but clients still receive the old build, the OCI
index has not picked the change up yet; check `tuna-os/flatpak-index` before
publishing anything else.

## 5. Tell users what to do meanwhile

Users are not stranded: finupdate is a front end. `bootc upgrade`,
`flatpak update` and `brew upgrade` on the command line keep working while it
is broken, so this is a degraded-convenience incident, not a stuck-system one.

Stopgap for anyone already on the bad build:

```sh
# Stop it re-updating to the bad build.
flatpak mask org.tunaos.finupdate

# Does the remote serve history? On an OCI-index remote it often does not.
flatpak remote-info --log tuna-os org.tunaos.finupdate

# Only if an older commit is listed above or is still deployed locally:
flatpak update --commit=<commit> org.tunaos.finupdate

# After the fixed build is out:
flatpak mask --remove org.tunaos.finupdate
flatpak update org.tunaos.finupdate
```

If no older commit is reachable, uninstalling is the only way off the bad
build until the republish lands.

## 6. After the incident

- Note in the follow-up issue which digest was bad, which was restored, and
  how long users were exposed (first publish finish time → rollback publish
  finish time).
- If the bad build passed `CI`, add the missing check to `ci.yml` rather than
  relying on review.

---

## Known gaps this runbook works around

These are limitations of the current release setup, not steps:

1. **No immutable release identity.** Only `latest*` tags are published, so a
   rollback target can only be named by digest or by matching timestamps
   between `gh run list` and the packages API. Tagged releases would remove
   this entire step.
2. **Publish is not gated on `CI`.** Both workflows start on the same push and
   neither waits for the other, so a commit that fails the test suite can
   still be published.
3. **Index refresh is out of band.** Re-pointing the registry tags is not the
   same as clients seeing the change; that depends on `tuna-os/flatpak-index`.
