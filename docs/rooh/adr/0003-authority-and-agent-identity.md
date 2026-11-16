# ADR 0003: Separate agent identities and scoped broker authority

Status: proposed technical decision, 2026-11-16.

## Context

Buzz channel membership controls visibility and posting. Project records and teams are groupings, not tool authorization. Broker-mode ACP currently requires explicit channel IDs and owner-only incoming messages. The broker can journal message effects and fence generations, but its public signed message omits task/generation/request IDs. Workflow approval execution does not yet persist and resume a paused run.

## Decision

Provision a separate Buzz identity for each Rooh agent capability. Keep signing keys outside the model runtime. Issue a broker credential bound to the logical agent, task scope and generation; restrict pilot membership to one explicit private channel and accept tasks from one named human owner. The broker mediates read/post/reply. A result replies to the exact signed task event, while a protected ledger retains the broker request ID and authority generation.

External effects require an action-level grant checked at the execution boundary. No agent receives payment, product write, PR merge, deploy or access-management rights from a channel role. The Figma full-frame confirmation and GitHub/release review remain external human gates. Do not use the incomplete Buzz workflow approval executor for these decisions.

When replacing a runtime, quiesce and drain its effects before fencing. An uncertain pending effect blocks successor work until reconciliation; do not claim exactly-once delivery. Agent-to-agent commands require a later explicit authorized-peer design and negative tests, not a relaxed owner-only switch.

## Consequences

- A compromised QA bridge cannot inherit Figma or release permissions through shared membership.
- The first pilot is limited to one owner and explicit channels; this is an intentional functional limit.
- Operator recovery must handle unresolved broker effects and preserve signed history.
- Future peer delegation needs sender identity, grant scope, depth/loop limits and revocation semantics.

## Alternatives considered

- Give all agents one shared key: simpler setup, but no attributable authority or contained compromise.
- Let any channel member command an agent: confuses visibility with authorization.
- Use workflow approval UI as the gate now: executor cannot yet prove durable suspend/resume.

## Revisit trigger

Extend the broker contract once a tested multi-owner or peer-delegation use case requires it. Preserve the separate identity and action-grant invariants.
