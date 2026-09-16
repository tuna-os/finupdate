# finupdate Roadmap

**Last updated**: 2026-09-16 | **Maintainer**: tuna-os (hanthor)

---

## Mission

Give TunaOS/Bluefin users a modern graphical system updater: one-click
`bootc`/`flatpak`/`brew`/`distrobox` updates behind a single pkexec elevation,
with live log streaming, cancel support, and reboot prompt — so keeping an
immutable system current is a first-class, polished desktop action, not a
terminal chore.

---

## Current Status

- **App**: GTK4/libadwaita in Rust (2024 edition), GNOME 47+.
- **Distribution**: shipped — `flatpak install tuna-os org.tunaos.finupdate`
  live on the TunaOS Flatpak remote (indexed in flatpak-index), OCI at
  ghcr.io/tuna-os/finupdate for x86_64 + aarch64.
- **Versioning**: **v0.1.0 release target pending** — zero tags in origin yet. The publish
  workflow fires on both `main` push and `v*` tags, so builds flow, but the
  OCI index requires a tagged release contract for downstream pinning.
- **Health**: active; core tag-parsing test coverage and STE workflow adoption complete (#116, #119).

### Priorities

| Priority | Item | Tracking | Status |
|----------|------|----------|--------|
| P0 | First tagged release (v0.1.0) — versioned OCI in the index | #65 | 🟡 In Progress |
| P1 | Unpin `flatpak-github-actions` in publish workflow | #64 | 🟡 Open |
| P1 | Cargo.lock: unmaintained `proc-macro-error2` | #63 | 🟡 Open |
| P2 | God-file refactor — module extraction | #62, #123 | 🟡 In Progress |
| P2 | ROADMAP-coverage entry in org ROADMAP tally | #1295 | 🟡 In Progress |

---

## Quarterly Goals

### Current Quarter (2026 Q3)

**Theme**: version the shipped app

| Goal | Owner | Tracking | Status |
|------|-------|----------|--------|
| Cut v0.x tag + first GitHub Release | hanthor | #65 | 🟡 In Progress |
| Unpin publish action | hanthor | #64 | ⬜ Not started |

### Next Quarter (2026 Q4)

**Theme**: quality and cadence

| Goal | Owner | Tracking | Status |
|------|-------|----------|--------|
| God-file reduction & module boundary refactor | hanthor | #62, #123 | 🟡 In Progress |
| Release cadence aligned with org (tagged builds in index) | tuna-os | #65 | ⬜ Not started |

---

*ROADMAP updated by strategist agent (ACMM L6 — full mode).*
