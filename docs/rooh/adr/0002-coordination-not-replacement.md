# ADR 0002: Buzz coordinates; existing harnesses execute

Status: accepted as product direction, 2026-11-16.

## Context

QA-07 already has a Trello watcher, browser constraints, receipt validation and Notion/Slack publication order. Figma2Code already has anchored A1/A2/A3 inputs, isolated G0 generation and a separate executor. Replacing either with a generic Buzz runner would duplicate domain policy and weaken proven input/review boundaries.

## Decision

Use Buzz for shared identity, private project conversation, bounded task requests, result projection and review history. A narrow QA bridge reads a completed QA watcher receipt and publishes a summary; a narrow Figma bridge invokes the existing bounded commands. The project harness remains responsible for domain execution and verdicts. Trello/Figma/GitHub remain authoritative for their respective task, design and code/release state in the pilot.

An agent may retain a Buzz identity and history while its model/process is event-driven and disposable. Do not add an always-running model loop merely to simulate presence.

## Consequences

- The pilot can be disabled without stopping QA-07 or Figma2Code.
- Buzz shows provenance and required decisions but does not silently change source task status.
- Cross-project handoff must use explicit capability contracts; a project channel or persona cannot infer execution rights.
- An adapter needs a small durable publication receipt, but no new global scheduler, general workflow engine or duplicate task database.

## Alternatives considered

- Central Buzz super-orchestrator: one place to run agents, but large migration and duplicated correctness rules.
- Notifications only: useful visibility, but no reliable task/result/evidence correlation or recovery.

## Revisit trigger

Consolidate execution only if multiple real harnesses share a stable, repeated contract and a measured duplication cost exceeds the migration and operational risk.
