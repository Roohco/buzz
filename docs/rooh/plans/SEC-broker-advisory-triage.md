# Broker dependency security slice

Status: scoped implementation plan, 2026-11-16. Base: `Roohco/buzz` main
`b39e44db1cedefd55fd3cd259053f3846ac89eae`. Owning Kanban card:
broker RustSec triage before a public pilot.

## Problem and outcome

The completed main Security job reports 39 RustSec IDs across the workspace.
Eight advisories intersect the broker's normal/build dependency tree. A public
pilot must not rely on an unreviewed vulnerable broker image. The outcome of
this slice is an evidence-backed remediation or explicit release stop for each
broker advisory; the broad workspace baseline is tracked separately.

## Scope lock

- **MUST now:** update semver-compatible broker dependencies (`rustls`,
  `rustls-webpki`, `webpki-roots`, `ctutils`); verify their patched versions in
  `Cargo.lock` and the broker tree; establish a concrete, tested resolution for
  the bundled SQLite advisory or keep public pilot blocked. Preserve the
  existing broker authority and effect-journal behavior.
- **NOT now:** add advisory ignores, replace SQLx or SQLite storage wholesale,
  change the broker protocol, or claim that fixing this tree clears all 39
  workspace advisories. Do not publish a new broker image until the exact
  source, dependency graph and security gate are verified.
- **Expected files:** `Cargo.lock` first; a minimal dependency declaration or
  upstream patch only if the SQLite bound requires it. No new service or store.
- **Estimate:** M, 1–3 engineering days for compatible updates and SQLite
  investigation; re-estimate before any SQLx vendor/fork change.
- **Local gate:** `cargo tree -p buzz-broker-host` against the affected packages,
  `cargo test -p buzz-broker-host`, targeted Clippy, `cargo deny check`, and
  repository `just ci` before PR. The existing 39-ID workspace baseline means
  the deny result must be compared by ID, not called green prematurely.
- **Release gate:** affected Rooh PR checks terminal; Rooh-built broker canary
  at an exact commit/digest; isolated R1 relay proof; no production deployment
  while any relevant advisory or R1 gate remains unresolved.

## Confirmed starting evidence

| Dependency | Locked | Advisories | Published patched version |
| --- | --- | --- | --- |
| `rustls` | 0.23.42 | 0285, 0346, 0348, 0349 | 0.23.46 |
| `rustls-webpki` | 0.103.13 | 0382 | 0.103.16 |
| `webpki-roots` | 1.0.7 | 0381 | 1.0.10 |
| `ctutils` | 0.4.2 | 0385 | 0.4.4 |
| `libsqlite3-sys` | 0.30.1, bundled | 0459 | 0.39.0 |

The broker tree does not enable rustls's non-default Brotli feature, so the
specific 0348 exploit path is absent in this build. The version still fails
dependency policy. `sqlx-sqlite` 0.9.0 requires `libsqlite3-sys >=0.30.1,
<0.38.0`, while the SQLite fix starts at 0.39.0; a lock-only update cannot
resolve 0459. This is a release stop until a compatible upstream SQLx release,
reviewed patch, or verified system-SQLite build is chosen and tested.

## Sequence and acceptance

1. Save the exact 39-ID baseline from main Security run `43847309678` and
   verify the eight broker intersections with `cargo tree`, including features.
2. Update the four compatible packages in a separate worktree. Confirm the
   lockfile diff contains only the expected compatible dependency movement;
   run focused broker/ACP tests and Clippy. If resolution or compilation
   fails, stop and identify the first incompatible dependency.
3. Resolve SQLite with the smallest supported option, backed by actual SQLx
   constraints, bundled/system library version, and broker tests. A proposed
   `deny.toml` ignore is not remediation. A new vendor/fork is a scope checkpoint.
4. Run the full local and remote gates, then repeat R1 against the exact image.
   Record the remaining workspace advisory IDs separately. Public pilot is
   permitted only after the broker-relevant list is zero or an owner accepts a
   specific documented exposure with a tested compensating control.

Rollback is the previous pinned `Cargo.lock` and broker image digest. Revert
the candidate before release if dependency resolution changes authority,
SQLite state compatibility, signed event handling or TLS verification.

## Local result, 2026-11-16

The lockfile now pins `rustls` 0.23.46, `rustls-webpki` 0.103.17,
`webpki-roots` 1.0.10 and `ctutils` 0.4.4. The necessary transitive movement is
`aws-lc-rs` 1.17.0 → 1.19.0, `aws-lc-sys` 0.41.0 → 0.47.0 and references to an
already locked `windows-sys` 0.59.0. No broker source or dependency declaration
changed. A focused lockfile assertion was red on the original lock and green
after the updates (`patched_version_failures []`).

`cargo check -p buzz-broker-host --locked`, broker host tests (36 passed,
2 ignored), targeted broker Clippy with `-D warnings`, and the repository's
complete `just ci` gate all passed locally. The complete gate exited 0 after
desktop, Tauri, web and mobile checks; mobile reported 1,835 passing tests.
`git diff --check` passed. The current host has no `cargo-deny` executable;
the previous local attempt to refresh its advisory database timed out, so
there is no local `cargo-deny` pass claim. Compare the exact IDs in the next
remote Security run against main run `43847309678`.

The seven TLS/certificate advisories in the table have patched lockfile
versions. `libsqlite3-sys` remains 0.30.1 with `bundled` enabled, so
RUSTSEC-2026-0459 remains a broker release stop. Published `sqlx-sqlite`
0.9.0 caps `libsqlite3-sys` below 0.38.0. Upstream SQLx commit
`10462fa4414c6c7f1ed1d39bb31678e3b726878c` raises that bound to admit
0.39.x, but pinning a Git source is a trust-boundary/scope checkpoint; it is
not part of this lockfile slice until the owner chooses it and its build and
runtime behavior are verified. Workspace advisories outside the broker tree
are separate follow-up work; this slice does not turn the Security job green.
