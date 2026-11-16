# ADR 0004: Source-versioned task and evidence contract

Status: proposed technical decision, 2026-11-16.

## Context

QA validity depends on the original task, attachments and dated references; a landing URL alone is input-incomplete. Figma validity depends on exact frame/root/version and receipt hashes. Current Buzz broker messages are signed ordinary channel events with no broker task/generation/request fields in the public event. Duplicate delivery and lost acknowledgement are expected failure modes.

## Decision

Represent a run by source system, opaque source ID, immutable source version, run ID, pinned harness revision, actor, generation, domain outcome, artifact hash and protected reference. Keep a protected adapter ledger mapping that record to Buzz task/result event IDs and broker request/receipt IDs.

For the first slice, use a signed owner task message and signed agent reply to that exact event; publish a short human-readable result with input revision, evidence reference and next decision. Do not invent a new public event kind before the job publisher, consumer and authorization path exist. Use `source system + source ID + source version + run ID + publication kind` as the bridge idempotency key. A retry queries/reconciles an accepted signed event before attempting another write.

Separate lifecycle state (`running/completed/blocked/failed`), domain outcome (`CHECKED/NOT_VERIFIED/generated_not_release_validated`) and review state (`pending/approved/rejected`). Never translate a generated candidate or missing QA input into a release/product pass. A correction is a new linked event; preserve the original signed history.

For cross-group work, add `pipeline_id`, parent task event ID, recipient capability, exact candidate SHA, preview bundle digest and stage attempt. QA acceptance/result must cite the candidate SHA and verified digest. A revision or changed served bundle requires a new QA task; previous QA proof cannot be reused. See [ADR 0006](0006-cross-group-handoff.md).

## Consequences

- One source revision and run are inspectable from Buzz without copying protected evidence into chat.
- The protected ledger is required for full broker-effect correlation until signed events carry suitable metadata.
- The first message format is deliberately small and human-readable; typed job events are a separately tested enhancement.
- The QA bridge needs a durable watcher-completion receipt because a browser worker receipt alone has no final Notion/Slack state.

## Alternatives considered

- Use titles or URLs as identity: mutable and incomplete.
- Two-way sync all task states: creates conflicting authorities and duplicate effects.
- Claim the broker journal itself provides public end-to-end provenance: its task/generation/request IDs are not embedded in the signed message.

## Revisit trigger

Adopt typed job events when a real job board needs queryable progress/cancel/result states and the public event contract can preserve authorization and provenance.
