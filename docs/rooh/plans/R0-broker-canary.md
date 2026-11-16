# R0 broker canary delivery plan

Status: implementation slice for the Rooh fork. Base: `Roohco/buzz` main `85903f77`; architecture and milestones are in [the merged roadmap](../ROADMAP.md).

## Scope lock

- **Problem:** the fork can run a docs-only PR check, but has no Rooh-built broker image with an exact source revision and OCI digest. Broker-path check routing is still unobserved.
- **Outcome:** a broker-image PR runs the broker and managed-agent checks; after merge, an operator can dispatch one main-bound workflow and record `ghcr.io/roohco/buzz-broker-host@sha256:…` plus the full source SHA and attestation.
- **MUST now:** record the source revision in the broker image; make PR smoke check that label; publish only from `Roohco/buzz:main` after smoke; report the pushed digest and attest it.
- **NOT now:** production rollout, real relay proof, multi-architecture image, version tags, generic image release framework, or changing broker authority behavior. Those belong to R1 or later.
- **Size:** S, 1–2 engineering days including review and first canary dispatch.
- **Files:** `Dockerfile.broker-host`, `.github/workflows/ci.yml`, `.github/workflows/managed-agent-smoke.yml`, `deploy/managed-agent/README.md`, this plan.

## Acceptance

1. A PR touching `Dockerfile.broker-host` gets terminal broker-host and managed-agent smoke jobs; PR jobs have no package-write token and publish no image.
2. The smoke image's `org.opencontainers.image.revision` label matches the checked-out `GITHUB_SHA`.
3. A manual dispatch on the fork's main branch first passes synthetic Compose smoke, then pushes `ghcr.io/roohco/buzz-broker-host:sha-<full SHA>`. It verifies that the registry tag resolves to the build digest and that the pulled image's revision label equals the source SHA, then prints and attests the digest.
4. A dispatch from a non-main ref or another repository cannot run the publish job. The production relay and existing managed agent stay untouched.

## Steps

- [x] Add `ARG VCS_REF=unknown` and the OCI revision label to the final image stage. Pass `GITHUB_SHA` from the existing smoke build and assert the label via `docker image inspect`.
- [x] Route `Dockerfile.broker-host` through the existing broker-host changed-path filter. Add a separate `publish` job to the smoke workflow with `needs: smoke`, a `workflow_dispatch` + fork-main guard, and job-scoped `packages`, `id-token` and `attestations` permissions. Use the pinned Buildx, login, build-push and attestation actions already used by the repository's image workflows. Document the exact dispatch and digest verification commands in the managed-agent README.
- [ ] Run `actionlint`, the Compose shell fixture, targeted broker checks and the repository `just ci` gate; record any environment-only failure rather than treating partial output as green.
- [ ] Push one reviewed slice, observe the PR's broker-host and managed-agent jobs, then merge after review. Dispatch from exact main, inspect the GHCR digest and attestation, and record both in the R0 Kanban card.

Local gate note, 2026-11-16: `actionlint` on the changed smoke workflow, Compose isolation fixture, 36 broker-host tests, local Docker image/label/CLI proof and a separate `just mobile-check` passed. The full `just ci` was attempted twice in this worktree. Its first run was interrupted during installation of the pinned Flutter SDK as disk space fell below 1 GiB; the SDK subsequently installed and mobile check passed. Its second run passed root Rust clippy and desktop/web static checks, then was interrupted during Tauri clippy when free space fell below 1 GiB. Full local CI is not green; the affected remote PR jobs are required before merge.

## Rollback

Do not dispatch or pull the canary tag. If a published image is defective, stop using that exact digest; no runtime is changed by this workflow. A later correction can supersede the tag, but the recorded digest remains the immutable audit reference.
