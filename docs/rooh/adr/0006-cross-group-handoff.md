# ADR 0006: Scoped cross-group handoff through a project channel

Status: proposed technical decision, 2026-11-16.

## Context

The target organization needs Figma2Code to produce a candidate, QA to verify that exact candidate, and the first agent to react to QA findings. Existing Buzz broker-mode ACP accepts owner messages only; current QA-07 checks prepayment landings and cannot be claimed as an independent Figma candidate reviewer. Shared membership in all group channels would expose unnecessary context without authorizing tools correctly.

## Decision

Keep Figma and QA group channels private. For one delivery project, create one additional private handoff channel with the human owner and only the participating Figma and QA agent identities. Cross-group tasks and results appear there as signed events; protected artifacts remain outside Buzz. Each handoff carries a pipeline ID, parent event ID, source/design revision, exact candidate SHA, requested recipient capability, budget/deadline, attempt and protected evidence references.

First prove `qa.review_candidate` under a human request against a real isolated preview and complete task/design input. Then implement one explicit grant allowing the Figma identity to request only that capability from the QA identity in that project. Recipient authorization checks grant, sender, channel, project, candidate identity and active generation before browser/model work. Use the existing job kinds when a complete publisher, consumer, cancel and status path is verified; otherwise the owner-mediated signed-message flow remains the limited pilot mode and is not called autonomous peer handoff.

QA returns a verdict tied to the candidate SHA. A new candidate invalidates the old verdict. A defect may trigger one automatic repair that cites the prior candidate and QA evidence, followed by another QA request; a second defect or uncertain effect stops for the owner. The Figma harness must first expose a safe bounded repair input if its current command cannot consume that feedback. Human review remains required before product writes or release. The Figma agent cannot validate its own candidate as QA.

## Consequences

- Agents can collaborate across groups without joining each other's internal channels.
- The handoff channel is a narrow, inspectable shared work surface, while action grants remain separate from membership.
- The organization MVP requires new QA capability and broker peer authorization; it is not delivered by the initial evidence-projection pilot.
- Pipeline identity, candidate hashes and bounded retry rules become mandatory to avoid stale QA results and unbounded Figma/QA loops.

## Alternatives considered

- Put both agents in both group channels: broadens read access and mixes internal discussion with cross-group contracts.
- Treat the existing QA-07 landing verdict as candidate QA: wrong input and validation contract.
- Use a free-form message from any peer as a command: bypasses broker owner-only protection and lacks a recipient grant.
- Introduce a global swarm coordinator: adds another scheduler and obscures which project harness owns a result.

## Revisit trigger

Create a broader inter-project routing model only after at least two real project handoff channels show a repeated need that cannot be handled by the same scoped grant and event contract.
