# ADR 0001: Rooh owns a separate Buzz fork

Status: accepted as product direction, 2026-11-16. Implementation status: the fork exists; no Rooh release is deployed.

## Context

Rooh needs project-specific integrations, identities and release cadence without placing internal harness contracts into the personal `malkovitc/buzz` fork or assuming upstream `block/buzz` will ship them. At the decision date `malkovitc/buzz` main is `53bc826`, while the currently deployed relay predates the Rooh integration.

## Decision

`Roohco/buzz` is the Rooh-owned fork. Its GitHub parent is `malkovitc/buzz` in the `block/buzz` fork network. Rooh-specific code, docs, CI and release tags live in `Roohco/buzz`. Production deployments must identify a commit and image digest built from this fork. Keep an explicit upstream remote for provenance and sync only reviewed commits, with normal tests and migration checks.

The repository is public because it is a fork of a public repository. Store no secrets, private Rooh task payloads, customer evidence, internal hostnames or operator instructions in it. Protected operational configuration and evidence belong in restricted systems.

## Consequences

- Rooh can review, release and roll back its own patch without waiting for upstream merge.
- Upstream fixes must be reconciled deliberately; the fork carries maintenance cost. Track the fork delta and remove Rooh-specific changes that upstream later supports.
- A green personal-fork PR, upstream tag or synthetic proof is not Rooh release evidence.
- CI must run against the Rooh branch and exact source revision before a Rooh-built image is promoted.

## Alternatives considered

- Continue development in the personal fork: lower setup cost but unclear company ownership and deployment provenance.
- Build a standalone coordination service: creates a second communication/control plane and duplicates Buzz.
- Fork `block/buzz` directly: possible, but loses the already reviewed broker and mobility changes in the current Rooh baseline and requires a larger immediate merge.

## Revisit trigger

Reconsider the fork only if the required Rooh contracts are upstreamed with equivalent authority and evidence guarantees and Rooh can release from that source without a separate patch.
