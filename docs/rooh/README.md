# Rooh AI Native Organization on Buzz

Status: proposed product and technical baseline, 2026-11-16. The architectural direction was approved for a dedicated Rooh fork; implementation and production rollout have not been approved by these documents.

This directory is the reviewable contract for the Rooh-owned `Roohco/buzz` fork. It describes a coordination layer around the existing QA Agent and Figma2Code/Strands harnesses. It does not describe capabilities as shipped merely because they appear in an upstream vision document.

| Document | Decision it supports |
| --- | --- |
| [PRD](PRD.md) | Users, outcomes, first product slice, scope and measurable acceptance |
| [Technical architecture](ARCHITECTURE.md) | Boundaries, contracts, data flow, authority, failure handling and rollout |
| [R2 live evidence](plans/R2-live-evidence.md) | First real QA-07 card projected to Buzz on the pilot |
| [Definition of Done](DOD.md) | Evidence required to call each slice complete and production ready |
| [Decision records and job telemetry](decision-records.md) | Reviewer reply format, label taxonomy, decision record and per-job telemetry fields |
| [Technical roadmap](ROADMAP.md) | Dependency order, delivery slices, local/remote gates and stop criteria |
| [Pilot deployment](PILOT-deploy.md) | What the pilot runs, identities by pubkey, channels, grant, operate/rollback, pending owner steps |
| [ADR 0001](adr/0001-rooh-owned-fork.md) | Rooh ownership, upstream relationship and release provenance |
| [ADR 0002](adr/0002-coordination-not-replacement.md) | Buzz coordinates; project harnesses execute |
| [ADR 0003](adr/0003-authority-and-agent-identity.md) | Identity, scoped broker authority and human gates |
| [ADR 0004](adr/0004-task-evidence-contract.md) | Source of truth, correlation, evidence and idempotency |
| [ADR 0005](adr/0005-protocols-and-rollout.md) | Pilot transport and criteria for richer protocols |
| [ADR 0006](adr/0006-cross-group-handoff.md) | Figma-to-QA pipeline across private agent groups |

## Evidence baseline

- Fork baseline: `Roohco/buzz` main at `53bc826e59824e2f5957aee74b8d1ac7f2482c39`, forked from `malkovitc/buzz` in the `block/buzz` network.
- Buzz has signed channel events, membership enforcement, ACP and limited broker read/post/reply. Job kinds exist, but no complete user-to-agent job dispatch and result flow. Broker-mode ACP accepts owner messages only; workflow approval execution cannot yet persist and resume a run.
- QA Agent revision `b126047f74b68b1e42b32ca435997ed8f7018071` had a healthy watcher and local checks at the 2026-11-14 handoff. A complete post-release card-to-Notion-to-Slack cycle still needs live proof.
- Figma2Code/Strands design evidence is pinned to upstream `develop` revision `8af4a66`. Generation-only G0 produces a candidate, not a release-validated product. The runner must pin and verify its own executable revision.
- The current QA-07 capability is a prepayment landing checker. Independent QA of a generated Figma candidate is a new, explicitly gated capability; the target cross-group pipeline is not shipped.
- The current production Buzz relay has no persistent Rooh broker or managed agent. Open voice and security PRs are not part of the fork baseline until merged and verified.

## Document rules

The PRD defines what users receive; architecture defines how and where authority resides; ADRs record durable choices; DoD requires observed evidence. In a conflict, first reconcile the documents and record the decision in an ADR. Each implementation slice needs its own small implementation plan and exact release revision.

These public-fork documents contain no credentials, private task payloads, customer evidence, internal hostnames or production connection details. Operational secrets and run artifacts remain in the relevant protected systems.
