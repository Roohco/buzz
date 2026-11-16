# R1 real-relay broker proof

Status: scoped implementation plan. Base: `Roohco/buzz` main `b39e44db`. Start the proof only after R0 records a verified broker image digest and attestation. The [roadmap](../ROADMAP.md) and [DoD](../DOD.md) define the milestone gate.

## Scope lock

- **Problem:** the existing keyless ACP test signs a reply against a synthetic HTTP relay. It does not prove that the real Buzz relay accepts, stores, authorizes and returns the owner task and broker-signed reply.
- **Outcome:** one isolated Rooh relay accepts a task from one owner in one private channel; a distinct keyless ACP identity reads it through the broker, replies to that exact event, and loses authority after quiesce, drain and fence.
- **MUST now:** pin relay, broker and ACP revisions/digests; verify the signed event IDs on write and read; deny an outsider; test exact retry/unknown ACK and one pending-effect outage; prove old-generation denial after fence; preserve non-secret operator evidence.
- **NOT now:** production relay cutover, Figma/QA adapters, peer delegation, typed job board, model quality, voice, Git effects or generic deployment tooling.
- **Size:** M, 3–5 engineering days. One real-relay proof is the vertical slice; add code only for a failure demonstrated on that path.

## Cheapest correct experiment

Reuse the repository's isolated relay and broker/ACP fixtures. Run one dedicated, disposable relay instance with its own Postgres, Redis, channel and identities. Preflight the existing harness project and ports before starting; never reset an active shared harness or connect the test to the Scaleway production relay. Use the deterministic ACP fixture for the first signed task/reply so model availability cannot mask a transport or authority failure. The relay must be the actual `buzz-relay` process backed by its normal stores, not the synthetic `/query` and `/events` fixture.

The R0 broker image is pinned by digest. Build or pin the ACP and relay from an exact Rooh commit and record their image/binary identity. If the existing isolated harness cannot be used without touching another workload, provision a separate named Compose project and ports for this proof; do not add a second permanent development stack.

The broad `main` Security job currently reports the same 39 RustSec IDs as the preceding Rooh main. Eight advisories touch dependencies in the broker tree. Keep R1 on an isolated, loopback-only relay while triaging those advisories as a separate release blocker; do not use this proof to authorize a public or production rollout.

## Acceptance and evidence

1. The owner creates a private channel and adds the agent. A signed kind-9 task carries a preallocated `run_id`, agent mention and channel UUID. The relay ACK and authenticated read return the same task event ID.
2. The broker issues one exact authority identity. The ACP receives no signing key, reads only the configured channel, and its signed `message.reply` has the task event as its NIP-10 parent. The relay ACK and subsequent read return the same reply ID. Protected evidence records the broker request ID and generation without recording the bearer or key.
3. An outsider cannot read the private channel or trigger the owner-only ACP turn. Changed request bytes under the same broker request ID conflict; exact retries return the same event ID and no second agent turn.
4. A lost relay ACK reconciles by querying the journaled event ID. During a relay outage, a pending message effect blocks fence and successor admission until an exact retry or operator drain resolves it. Record the unresolved state if the relay's timestamp window prevents resolution; do not edit broker state to force progress.
5. Quiesce stops new turns. After drain and fence, a stale credential and old generation cannot publish a new message. Re-enable work only with a fresh generation after the prior effect is terminal.

Evidence bundle: exact source commits and image digests, channel UUID, task/reply event IDs, broker request ID and generation, signed-event verification, denial/retry/outage/fence results, commands and logs with secrets removed, and teardown confirmation. Synthetic Compose smoke remains supporting evidence, not the R1 result.

## Execution order

1. Verify the R0 digest/attestation and inspect free disk, active containers and port ownership. Select an isolated relay instance; record its exact source revision or image digest and health endpoint. Confirm that no existing harness or production resource will be reset.
2. Run the smallest happy path: owner channel/member/task → broker issue/serve → keyless ACP deterministic reply → authenticated relay read. Stop and locate the first broken boundary before adding a bridge or new protocol.
3. Exercise the denial, duplicate, lost-ACK, outage and lifecycle cases against the same real relay. Reuse existing broker receipts and tests where possible; add a focused regression only if the live run exposes a code defect.
4. Run `just ci` before any code PR and focused broker/ACP/relay tests. Record environment failures separately. Review one completed slice, then merge only with applicable remote checks terminal. Verify the exact remote main revision.
5. Teardown only the proof-owned resources, preserve sanitized evidence in the private R1 Kanban card, and mark R1 done only when every acceptance item is demonstrated. If an effect remains unresolved, leave successor admission closed and record R1 blocked at that boundary.

## Rollback and decision

Stop the proof-owned ACP and broker, quiesce/drain/fence its generation, and remove only the disposable relay resources created for R1. Production Scaleway relay, QA-07 and Figma2Code continue on their current revisions. Any need for a new broker action, cross-group grant or permanent orchestration service is a scope checkpoint rather than an implicit R1 expansion.
